# Roadmap

holzWave is a terminal emulator for macOS, Windows, Linux, Android and the browser, written from scratch around one shared
Rust core. [Wave Terminal](https://github.com/wavetermdev/waveterm) is the UX reference, not the code base. If code is
ever taken over, its Apache-2.0 licence is checked first and the notices are kept.

> [!NOTE]
> This is a plan, not a status, except for the "Status" paragraph of Phase 2, which says what is built. The knowledge
> about `libghostty` and Apple's Foundation Models framework dates from mid-2026; **both are re-checked against the
> current documentation in phase 1.**

## Platforms

| | macOS | Windows | Linux | Android |
|---|:---:|:---:|:---:|:---:|
| Local shell | ✅ | ✅ | ✅ | ✅ |
| SSH | ✅ | ✅ | ✅ | ✅ |
| WSL / Docker | – | ✅ | ✅ Docker | ❌ |
| `wsh`, K8s, Talos, Azure, GAM7 | ✅ | ✅ | ✅ | ❌ |
| Sync, secret store | ✅ | ✅ | ✅ | ✅ |
| Apple on-device AI | ✅ macOS 26+, Apple Silicon | – | – | – |

The `wsh`, K8s, Talos, Azure and GAM7 row means built and tested against fake tools and recorded fixtures. The integrations
did not run on Windows or Linux hardware, the Flatpak build does not run them, and a real cluster or account is open on
macOS too (Part 9 of [test-plan.md](test-plan.md)).

The Android column means built and run on arm64 emulators of Android 12, the newest Android and an image with 16 KB memory pages,
never on a phone or a tablet. Its Docker, WSL, `wsh` and integration cells are `❌` because the build leaves them out, and what no
emulator can show is open in Part 13 of [test-plan.md](test-plan.md).

The browser is a platform too, through the web build (phase 11): the same window served by one container, built and tested in containers, with its rows open (Part 10 of [test-plan.md](test-plan.md), [server.md](server.md)). It is not a column above because the table lists what each desktop or mobile app does; the browser build has its own limits (see the web build lines in [status.md](status.md)).

Not included on any platform: a web browser block. iOS is not planned (no Apple developer account).

## Phase 1 — Spike (one week)

Goal: decide the terminal engine path and the renderer, with numbers.

- Bind `libghostty-vt` from Rust (FFI or `libghostty-rs`).
- Read PTY output, parse it, and render it in a Tauri window.
- Decide the renderer: ghostty compiled to WASM in the webview, or an own WebGL renderer.
- Record a baseline: throughput, input latency, memory.

Done when: a shell runs in the window, the renderer is chosen with the measurements in the repository, and the
`TerminalEngine` trait has been exercised by at least the primary engine.

## Phase 2 — Desktop MVP

Status in `0.0.1-beta2` (macOS, Windows and Linux; preview packages are on the GitHub releases page): implemented and
covered by the automated suites. What no suite can see is the human checklist in [test-plan.md](test-plan.md), which the
owner runs on real machines.

Exists: the key encoder with Kitty keyboard and application modes (Option is not Alt by default), engine replies,
dirty-row frames with sequence numbers, bounded panes that end their shell when closed, repaired layouts, three
themes and user themes, the Terminal-modern chrome, mouse reports and the wheel policy for full-screen programs,
selection tracked in the core, scrollback search, links, hints and copy mode, the command palette, the settings
dialog and key binding editor, accessibility preferences, restore of directories and window frame, and a test that
there is no telemetry and no account. The file formats are in [config.md](config.md), the measured budgets in
[architecture.md](architecture.md) section 12.

Does not exist yet:

- **Phase 2.1:** drag-and-drop of panes, tear-off into a new window (DESK-20, DESK-29), smooth scrolling and cursor trail,
  optional and off under reduced motion (DESK-28).
- **Phase 2.2:** Windows (ConPTY, WebView2, installer) and Linux (X11 and Wayland, Flatpak, AppImage, deb and rpm),
  with their own default key tables and the OS-driven UI font size (DESK-30, DESK-31). The owner runs the per-platform
  list of [test-plan.md](test-plan.md) on their own hardware (D-22). `0.0.1-alpha1` to
  `0.0.1-alpha3`, `0.0.1-beta1` and `0.0.1-beta2` are released (D-21). **Status: written and cross-checked from the Mac, and run by the owner on Windows and Linux machines.** The Windows runtime (ConPTY shell, PowerShell directory tracking, exit poll, launchers, AltGr and the per-OS key
  tables, the OS text size), the Linux runtime, a per-user unsigned NSIS installer configuration and the deb, rpm and
  AppImage configuration exist; the code compiles for both platforms from the Mac, the Linux tests pass in a container and
  a deb, an rpm and an AppImage were built there. The build guides are [build-windows.md](build-windows.md) and
  [build-linux.md](build-linux.md), the owner's list is Part 4 of [test-plan.md](test-plan.md) (rows W1 to W14 and L1 to
  L11), and the differences from macOS are listed once in [design.md](design.md) section 7. The Flatpak is
  the last plan of the phase and conditional: its manifest and the host-shell code path exist and are checked from the Mac
  ([flatpak.md](flatpak.md)), but it was never built or run, and it ships only if it builds on the owner's machine and row
  L10 passes. DESK-30 and DESK-31 are marked done only after the owner's rows pass.
- **Own window frame:** on Windows and Linux the page's bar is the window bar, with three window buttons in the style of the system
  (flat on Windows and most Linux desktops, round on GNOME, with the layout read from GNOME's `gsettings`); macOS keeps its overlay
  bar, and the Windows 11 snap layouts are not built. **Status: written and tested against a mock window in WebKit and Chromium
  and by compile checks for Windows and Linux; it has never been seen on a real Windows or Linux window.** Drag, the resize edges,
  the buttons, the drawn states and the behaviour under Wayland are unverified. Under Wayland the setting `Frame` on `Auto` is
  the own frame, which was not tried there; `Frame` set to `System` in Settings, or `window_frame.json` edited by hand, gives
  the system's title bar back ([config.md](config.md)). The frame choice and the Linux button style are per-device settings
  and never sync. The keyboard reaches the bar with `F6` (tested with Playwright's keyboard only, [design.md](design.md) section 6.1). The list of what is open is in [design.md](design.md) section 7.

- Local shell, scrollback, copy and paste, Unicode, resize, themes.
- Block tiling with tabs and workspaces.
- Persistent layout state.

## Phase 3 — Remote

Status: built in the core and the page, covered by tests against an in-process SSH server. Not yet run against real
hosts, WSL or Docker on a real machine, or the OS keychain (rows C1 to C6 of [test-plan.md](test-plan.md)).

- ✅ SSH manager with reconnect (`core/src/ssh.rs`, `core/src/connections.rs`), jump host, host key check (trust on first use). The terminal, the files and the commands of a saved connection share one login (`core/src/pool.rs`).
- ✅ Secret store on the OS keychain (`core/src/secrets.rs`, feature `keyring`).
- ✅ WSL and Docker as connection types (a local pane that runs `wsl.exe -d` or `docker exec -it`).

## Phase 3b — Sync

- ✅ Sync core: WebDAV (ureq), XChaCha20-Poly1305 + Argon2id, merge engine ([sync.md](sync.md)). App layer ✅: account in the keychain, scheduled runs, title bar button and popover; downloads are applied at the next start.
- End-to-end encryption (passphrase; `age` or XChaCha20). **The format is fixed first**, because it determines the
  data format of everything else. See [sync.md](sync.md).
- Conflicts: a version stamp per file, last writer wins, the old version is kept as a backup.
- Secrets are synced only in encrypted form.

## Phase 4 — Previews

Status: file browser, Markdown, text, image, video and audio previews are built (local and over SFTP), covered by tests
against an in-process SSH and SFTP server and the page stub; not yet run on real hosts (rows P1 to P5 of
[test-plan.md](test-plan.md)).

- ✅ File browser (local and SFTP), Markdown, text, image, video, audio.
- ✅ PDF (PDF.js, vendored, read in ranges through the core, drawn to a canvas).
- ✅ Code editor (CodeMirror 6, vendored bundle in a shadow root): text files and Markdown source, syntax colours, Cmd/Ctrl+S saves locally and over SFTP, a change on disk is noticed, closing asks when changes are unsaved.

## Phase 5 — `wsh` CLI

- ✅ A small Rust binary (`wsh/`) that controls the app over a Unix socket or a named pipe (`ipc/`). The socket is in a
  folder only the user can enter, and every request carries a secret that only the panes of this run of the app know
  (`HOLZWAVE_IPC`, `HOLZWAVE_TOKEN`, `HOLZWAVE_PANE`, set for local panes; `wsh` beside the app is put on their `PATH`).
- ✅ Commands: `view <file>` and `edit <file>` (a preview beside the pane; Markdown opens as a page, `e` edits it),
  `setmeta title <text>`, `ping`, `hist` and `init <shell>` (the history hooks and the `^R` binding of Phase 6) and `ai <prompt>`:
  ask-only, text piped in is sent along, the window confirms before anything leaves the computer, and the answer streams
  into the pane with the characters a terminal could act on removed.
- ✅ `wsh` on a Linux host through an SSH tunnel: a saved SSH connection opts in with `Use wsh on this host`; its panes then
  get a private Unix socket forwarded back over the connection's own login (`HOLZWAVE_SOCKET`, `HOLZWAVE_TOKEN`,
  `HOLZWAVE_PANE`), and `view`, `edit`, `setmeta title` and `ping` work for a file of that host over SFTP, while `ai`, the history
  commands and everything that decides are refused there. The palette action `Install wsh on this host` uploads the bundled
  static build (Linux, `x86_64` and `aarch64`) to `~/.holzwave/bin` and checks it. Tested against a test server and against a
  real OpenSSH on loopback with the build of this computer; the owner has not run it on a real Linux host yet.
- ✅ The macOS, Windows and Linux packages made by the release scripts carry `wsh` beside the app (not the Flatpak).
- Open: `wsh` in WSL and Docker panes (they have no SSH session to forward over), on hosts that are not Linux, an uninstall action
  and an update check at connect.

## Phase 6 — Extras

Status: built in the core and the page and covered by tests against in-process servers, scripted providers and a page
stub. Not yet run with real accounts, or a real local model server; the
manual rows are in Part 7 of [test-plan.md](test-plan.md).

- ✅ Sidebar with hosts, history search and widgets (note, Pomodoro timer, command/link list; custom ones from `widgets/*.json`, validated, no code). The files are described by [widget.schema.json](widget.schema.json) and a test keeps the schema and the loader in step. The Context section is built with the integrations (Phase 6b). Open: widgets on mobile.
- ✅ History with fuzzy search: SQLite (`history.db`, mode 0600, `secure_delete`), fzf-style matching (`nucleo-matcher`), fed by shell hooks through `wsh` (`wsh init bash|zsh|fish`; PowerShell panes report on their own), a best-effort filter keeps secrets and leading-space commands out, a dialog with `Cmd/Ctrl+Shift+H` and a sidebar section. `^R` is an opt-in binding (`wsh init <shell> --ctrl-r`). Open: remote hosts (a command from a pane on a saved connection is found from that pane, but nothing typed on the far side is recorded: the tunnel refuses the history commands on purpose, so a remote host cannot write into the history), optional sync (deletions of themes and widgets travel, but the history is a database that cannot be carried entry by entry, so a forgotten command would come back; it stays local until deletions of single entries can be synced).
- ✅ System monitor block (CPU, memory, network, busiest processes; `sysinfo`). Other computers over SSH: built for Linux hosts (a fixed read-only command through the exec session of a saved connection, names and numbers only; another system is shown as not supported).
- ✅ AI chat behind a provider abstraction: Anthropic, OpenAI, Gemini, local servers (Ollama, LM Studio, llama.cpp), AI gateways and the Apple model. Four surfaces: a chat pane, the sidebar column, the palette actions and `wsh ai`. Terminal context is sent only with consent, per pane; every tool action needs your approval; keys live in the OS keychain; none of it is synced.
- ✅ Apple AI provider: a Rust crate with a Swift package behind a small C interface (Foundation Models, macOS 26+, Apple Silicon, Apple Intelligence switched on), linked weakly and registered only where it is available. Otherwise it cannot be chosen and you choose another provider: there is no silent fallback. It answers on the Mac itself, offline, with no account and no key. Open: the launch on a Mac below macOS 26 (rows AI-A1 to AI-A4 of the test plan).

## Phase 6b — Native integrations (desktop)

One compiled-in description for each tool ("CLI integration"): binary detection, context switcher, output parser, block. No
plugin host: the two published JSON documents, the profile file and the view model, are data with a schema.

| Integration | Approach |
|---|---|
| Kubernetes | `kubectl` wrapper (`kube-rs` is not used: 34 more crates and exec plugins to rebuild): context and namespace switcher, pod list, logs, "exec in pod" as a terminal block, and a shell pane with its own context. Built |
| Talos | `talosctl` wrapper: nodes, services, machine status, info and contexts as views; dmesg and the dashboard as terminal blocks; making a context the default behind a card (no gRPC client). Built; the output shapes are written from the documents, not recorded |
| Azure CLI | `az` wrapper: account, subscriptions and resource groups, the subscription switcher, login state in the status line, `az login` as a terminal block. Built |
| GAM7 | wrapper with the choice of a configuration section (several domains and configs), sections, domain and users as a table or JSON block. Built; the text of `gam showsections` is an assumption |

Connection data goes through the profile file and the keychain, and sync carries the profiles and their small secret
variables inside the encrypted payload; nothing is imported from a kubeconfig, a talosconfig or a tool's token files.

Status: built, rows open (test-plan Part 9). Built and tested against fake tools and recorded fixtures; not yet run against
a real cluster, a real Talos, Azure or GAM account, or on Windows and Linux hardware. The design is in architecture.md section 9.

Open: the New block picker (drawn in the design, not built), a saved Pod connection kind, integration tools for the AI chat
(none, on purpose), importing a kubeconfig, user-loadable integrations, and `kube-rs` or a Talos gRPC client only if polling
proves too slow.

## The web build (after 0.0.1)

holzWave in a browser from one container, for a person or a small team that runs its own server.

- In: one server binary with a supervisor, one worker and one user id for each person, a small privileged helper with four file
  capabilities, TLS 1.3 from files, ACME or a proxy, sign-in by passkey, password and authenticator code or single sign-on,
  invitations, roles and an audit log, an encrypted store of each person's saved passwords, the same commands and the same page as
  the desktop over one WebSocket, upload by drop and download of the files the core grants, the Docker image, a Compose file and a
  Helm chart, and an operator guide ([server.md](server.md)).
- Out: shared panes between people, an installable web app, a mobile layout of the page, a token for scripts or other programs
  (the device token of an app, below, is for that app only), an operator for Kubernetes, tear-off windows, the Apple on-device
  model, the local network list and self-update.
- Deferred: compression of the WebSocket (the measured library saved 79 to 94 percent but left 18 Autobahn cases unimplemented, so
  it is off), workers that survive a restart (a restart ends every pane), user namespaces as a stronger boundary, a binary wire
  format, more than one replica, a drop target on the chat pane and the file browser as well as the shell pane, and a refusal sentence of its own for each reason a download is refused.

Status: built, rows open (test-plan Part 10). Built and tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider, hardware authenticators or real networks. The design is in architecture.md section 16.

## Server sync and remote sessions (after the web build)

The holzWave server for the apps: the target of the encrypted sync, and terminal sessions that keep running on the server.

- In: the sign-in of the desktop and Android apps as a device through the person's browser (the device authorization grant,
  a token in the OS keychain, only its hash on the server, an end after 90 days without use by default, revocation at once), the lists of
  devices for a person and an administrator, two switches of the operator (`Sync target` and `Remote sessions`, off by default,
  with a ceiling in the configuration), the server as a second sync transport for the same encrypted files with per-person
  bounds, and remote sessions: shells in the person's worker that outlive every client, attached over one bounded socket with
  the screen and a tail of the scrollback, one writer at a time with `Take over`, shown in a pane of the apps and of the browser
  window ([server.md](server.md), [sync.md](sync.md)).
- Out: sharing a session with another person, a second server, two sync targets at once, a palette entry for sessions and a
  key for `Take over`.
- Deferred: a device token bound to the device (DPoP or mutual TLS) or rotated (the reason is in [server.md](server.md)),
  moving the sync files from one target to the other, and workers that survive a restart (a restart ends every remote session).

Status: built; Part 10 of [test-plan.md](test-plan.md) names what needs a real device or server. Built and tested against the
real server in one process over the loopback address, against a mock of
the app in WebKit and Chromium, and in real browsers against the real server, on the author's Mac; never run on a real phone or
a real desktop against a real server over a real network, with a real passkey or single sign-on approving a device, behind a real
reverse proxy for the device socket, with two real people, on Android, or across a server restart under a real workload. The
design is in architecture.md section 16.

## Phase 7 — Mobile

Status: built, rows open (test-plan Part 13). Android was built and run on arm64 emulators of Android 12, the newest Android and an
image with 16 KB memory pages, never on a phone or a tablet. **Built:** the app for arm64 only; the system shell in the app's private
folder; SSH; the secret store behind the Android Keystore with the system prompt; HTTPS through the system's trust store; the touch page
(one pane per screen, the key row, selection by long press, scrolling, the pinch) and the tablet layout; one-file hand-over through
the system's document dialog; the clipboard through the app's own code; and a foreground service with a `Stop all` notification.
**Not tried:** everything that only a phone, a keyboard, a sensor, a network, a storage provider or a person shows, listed as rows
with empty results in Part 13 of [test-plan.md](test-plan.md); how it is built and what each emulator run showed is in
[build-android.md](build-android.md). Two defects found on an emulator are fixed and were checked on emulators only (the bottom bar that lay under the system buttons with
three-button navigation on Android 12, and the service not coming back after the system recreated the window).

- Android: local shell plus SSH. iOS is out of scope (no Apple developer account).
- Touch UI, a soft keyboard with special keys, a simplified layout (one block per screen).
- Dropped on mobile: Docker/WSL, a local `wsh`, and the K8s/Talos/Azure/GAM7 wrappers (only where it makes sense).
- Sync and the secret store work on mobile too.
- The AI chat sends on Android once the secret store is unlocked, and the cloud providers are part of the Android build. It was tried
  against test servers and scripted providers only, never against a real provider from a phone.

## Phase 8 — Polish

Status: built, rows open (test-plan Part 11). **Built:** the Kitty graphics protocol at all three levels (still pictures, animations,
pictures drawn with placeholder characters) and OSC 8 hyperlinks, in the core, the server, both renderers and the page, with the
link target in the status line, a question when a link's text names another place, and a list of schemes that a click may open
([architecture.md](architecture.md) sections 6 and 8). **Measured:** a baseline of the core, the server and the page in two browsers was
recorded before this work and again after it, on the author's Mac and in a Linux container, and `sh scripts/perf-check.sh` judges by
those numbers with no fixed target ([architecture.md](architecture.md) section 12); Windows and the desktop window's key to glass are
not measured. **Signed:** the Linux packages can be signed by the owner with minisign and no release has been signed yet; the
Windows installer and the macOS app stay unsigned ([signing.md](signing.md)). **Not tried:** a real program that sends pictures or
links, the desktop window by an automated test, Windows, and a signed release on a real distribution; the list and the rows are in
Part 11 of [test-plan.md](test-plan.md).

The aims as first written, kept as the history they are:

- Kitty graphics protocol, hyperlinks.
- Performance tuning.
- Packaging and signing on every platform.

## Risks

| Risk | Answer |
|---|---|
| The `libghostty` API is young and may change | Everything sits behind the `TerminalEngine` trait; `alacritty_terminal` is the fallback |
| Apple AI is reachable only from Swift | A small Swift package behind a C interface in a crate of its own (macOS only, linked weakly), and the provider is optional |
| The sync encryption format cannot change later without a migration | Decide it before any sync code is written |
| AI and secrets | Context is never sent without consent, and the page never sees the text of a chip; secrets live only in the OS keychain |
