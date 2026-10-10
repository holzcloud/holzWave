# Roadmap

holzWave is a terminal emulator for macOS, Windows, Linux, iOS and Android, written from scratch around one shared
Rust core. [Wave Terminal](https://github.com/wavetermdev/waveterm) is the UX reference, not the code base. If code is
ever taken over, its Apache-2.0 licence is checked first and the notices are kept.

> [!NOTE]
> This is a plan, not a status. Nothing below is built. The knowledge about `libghostty` and Apple's Foundation
> Models framework dates from mid-2026; **both are re-checked against the current documentation in phase 1.**

## Platforms

| | macOS | Windows | Linux | iOS | Android |
|---|:---:|:---:|:---:|:---:|:---:|
| Local shell | ✅ | ✅ | ✅ | ❌ sandbox | ✅ |
| SSH | ✅ | ✅ | ✅ | ✅ | ✅ |
| WSL / Docker | – | ✅ | ✅ Docker | ❌ | ❌ |
| `wsh`, K8s, Talos, Azure, GAM7 | ✅ | ✅ | ✅ | ❌ | ❌ |
| Sync, secret store | ✅ | ✅ | ✅ | ✅ | ✅ |
| Apple on-device AI | ✅ macOS 26+, Apple Silicon | – | – | ❓ | – |

Not included on any platform: a web browser block.

## Phase 1 — Spike (one week)

Goal: decide the terminal engine path and the renderer, with numbers.

- Bind `libghostty-vt` from Rust (FFI or `libghostty-rs`).
- Read PTY output, parse it, and render it in a Tauri window.
- Decide the renderer: ghostty compiled to WASM in the webview, or an own WebGL renderer.
- Record a baseline: throughput, input latency, memory.

Done when: a shell runs in the window, the renderer is chosen with the measurements in the repository, and the
`TerminalEngine` trait has been exercised by at least the primary engine.

## Phase 2 — Desktop MVP

- Local shell, scrollback, copy and paste, Unicode, resize, themes.
- Block tiling with tabs and workspaces.
- Persistent layout state.

## Phase 3 — Remote

- SSH manager with reconnect.
- Secret store on the OS keychain.
- WSL and Docker as connection types.
- One connection abstraction that terminal *and* previews use.

## Phase 3b — Sync

- A sync module in the core (`opendal` or `reqwest_dav`); an endpoint plus an app password.
- End-to-end encryption (passphrase; `age` or XChaCha20). **The format is fixed first**, because it determines the
  data format of everything else. See [sync.md](sync.md).
- Conflicts: a version stamp per file, last writer wins, the old version is kept as a backup.
- Secrets are synced only in encrypted form.

## Phase 4 — Previews

- File browser; Markdown, image, video and PDF.
- Code editor (Monaco) with local and remote file access over SFTP.

## Phase 5 — `wsh` CLI

- A small Rust binary that controls the core over a Unix socket or a named pipe.
- Commands: `view`, `edit`, `setmeta`, `ai`.
- On remote hosts through an SSH tunnel.

## Phase 6 — Extras

- Widgets sidebar and custom widgets described by a JSON schema.
- History with fuzzy search (SQLite).
- System monitor block.
- AI chat behind a provider abstraction (local and cloud). Terminal context is sent only with consent.
- Apple AI provider: a Swift bridge as a Tauri plugin (Foundation Models, macOS 26+, Apple Silicon). Selectable only
  on supported Macs, with a fallback otherwise.

## Phase 6b — Native integrations (desktop)

One plugin schema, "CLI integration": binary detection, context switcher, output parser, block widget.

| Integration | Approach |
|---|---|
| Kubernetes | `kube-rs`: context and namespace switcher, pod list, logs, "exec in pod" as a terminal block |
| Talos | gRPC (`tonic`) or a `talosctl` wrapper: node status, logs, dashboard block |
| Azure CLI | `az` wrapper: subscription switcher, login state in the status bar |
| GAM7 | wrapper with profile choice (several domains and configs), output as a table or JSON block |

Connection data goes through the secret store and sync.

## Phase 7 — Mobile

- iOS: SSH only. Android: local shell plus SSH.
- Touch UI, a soft keyboard with special keys, a simplified layout (one block per screen).
- Dropped on mobile: Docker/WSL, a local `wsh`, and the K8s/Talos/Azure/GAM7 wrappers (only where it makes sense).
- Sync and the secret store work on mobile too.

## Phase 8 — Polish

- Kitty graphics protocol, hyperlinks.
- Performance tuning.
- Packaging and signing on every platform.

## Risks

| Risk | Answer |
|---|---|
| The `libghostty` API is young and may change | Everything sits behind the `TerminalEngine` trait; `alacritty_terminal` is the fallback |
| The iOS sandbox allows no local shell | iOS is SSH-only by design |
| Apple AI is reachable only from Swift | A bridge plugin, and the provider is optional |
| The sync encryption format cannot change later without a migration | Decide it before any sync code is written |
| AI and secrets | Context is never sent without consent; secrets live only in the OS keychain |
