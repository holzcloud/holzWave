# Architecture

Status: **plan**, with the parts built in `0.0.1-alpha1` marked ✅. The layout of crates and the boundaries
between them are fixed; the contents of the later phases are a plan and will move.

## 1. Goals and non-goals

**Goals**

- One terminal core for macOS, Windows, Linux, iOS and Android. Platform shells stay thin.
- Fast and small: one draw call per pane and frame, a few MiB of RAM per pane, input latency below one frame.
- Replaceable parts: the VT engine, the connection type, the renderer, the AI provider and the sync backend each sit
  behind a narrow interface.
- Secure by default: secrets only in the OS keychain, the terminal's content only leaves the device with consent,
  the webview has no more power than it needs.
- Few dependencies. Security-critical code (SSH, cryptography, VT parsing) is never hand-written.

**Non-goals**

- A web browser block.
- A local shell on iOS (the sandbox forbids it).
- Being a general plugin host. Integrations are a fixed, reviewed set (section 9), not arbitrary third-party code.

## 2. System context

```
              ┌────────────────────────── webview (TypeScript, no framework) ──────────────────────────┐
              │  chrome: tabs · workspaces · dividers · overlays        renderer: WebGL2 (Canvas 2D)   │
              └────────────▲─────────────────────────────────────────────────────▲─────────────────────┘
                           │ commands (invoke)                                   │ events: frame, layout
              ┌────────────┴──────────────── app shell (Tauri 3) ───────────────────┴─────────────────┐
              │  command layer · window · menus · capability scoping · plugins (keychain, Apple AI)    │
              └────────────▲──────────────────────────────────────────────────────────────────────────┘
                           │ library calls
 ┌─────────────────────────┴────────────────────── holzwave-core (Rust) ────────────────────────────────┐
 │  layout      workspaces · tabs · split tree · geometry                                  ✅             │
 │  engine      TerminalEngine trait ── libghostty-vt │ alacritty_terminal (fallback)      ✅ (ghostty)  │
 │  pty         local shell over portable-pty                                              ✅             │
 │  conn        Connection trait: local · ssh (russh) · wsl · docker                       phase 3        │
 │  secrets     SecretStore trait over the OS keychain                                     phase 3        │
 │  store       SQLite: settings, history, profiles                                        phase 2/7      │
 │  sync        encrypted WebDAV sync, conflict handling                                   phase 4        │
 │  preview     file browser, markdown, media, editor backends                             phase 5        │
 │  control     socket server for wsh                                                      phase 6        │
 │  ai          provider trait: local · cloud · Apple Foundation Models                    phase 7        │
 │  integrate   CLI-integration schema: kubectl · talosctl · az · gam                      phase 8        │
 └──────────────────────────────────────────────────────────────────────────────────────────────────────┘
                           ▲
                    wsh (Rust binary) ── Unix socket / named pipe ── also over SSH tunnels
```

The rule that makes this work: **the core owns all state; the webview mirrors it.** The webview holds no
authoritative layout, session or setting, and can be reloaded at any time.

## 3. Workspace layout

| Path | Crate / package | Role | State |
|---|---|---|---|
| `core/` | `holzwave-core` | everything above except the shell | ✅ engine, pty, layout |
| `app/src-tauri/` | `holzwave-app` | Tauri command layer, window, plugins | ✅ |
| `app/src/` | TypeScript | chrome, renderers, input | ✅ |
| `spike/` | `holzwave-spike` | measurement tool, not shipped | ✅ |
| `wsh/` | `holzwave-wsh` | CLI, depends on a small `holzwave-control` client crate | phase 6 |
| `plugins/apple-ai/` | Swift package | Foundation Models bridge | phase 7 |
| `docs/` | | roadmap, architecture, design, sync | ✅ |

`holzwave-core` stays free of Tauri and of any UI type. It must build and test on its own, and it is the crate the
mobile shells link.

## 4. Core boundaries

Each boundary is a trait in `holzwave-core` with at least one real implementation and a test double.

| Trait | Methods (sketch) | Implementations | Why a boundary |
|---|---|---|---|
| `TerminalEngine` ✅ | `feed`, `resize`, `scroll`, `screen`, `take_replies`, `encode_key`, `set_theme` | `GhosttyEngine` ✅, `AlacrittyEngine` | `libghostty-vt` is young and its API moved already between 0.2.2 and master |
| `Connection` | `open_shell(size) → Stream`, `open_files() → FileSystem`, `status()` | local, ssh, wsl, docker | terminal and previews share one login |
| `SecretStore` | `get`, `set`, `delete` | macOS Keychain, Windows Credential Manager, Secret Service, iOS/Android keystores | secrets never reach a file |
| `SyncBackend` | `list`, `get`, `put`, `delete` with ETag | WebDAV (`opendal` or `reqwest_dav`) | Nextcloud today, others later |
| `AiProvider` | `chat(messages, context) → stream` | local server, cloud APIs, Apple Foundation Models | consent and context rules live above it |
| `Integration` | `detect`, `context()`, `parse(output)`, `block()` | kubernetes, talos, azure, gam | one schema for four tools |

The engine trait is **not `Send`**: `libghostty-vt` objects are `!Send + !Sync`. An engine therefore lives on one
thread and is driven over a channel (section 5). This shapes the whole concurrency model.

## 5. Concurrency model

```
 per pane                                                                               shared
 ┌────────────┐ bytes  ┌──────────────────────────────────────────┐ frame ┌────────────┐
 │ PTY reader │───────►│ engine thread  (owns Engine + PTY writer) │──────►│ emitter    │──► webview
 │  thread    │        │  loop: input · output · resize · scroll   │       └────────────┘
 └────────────┘        └──────────────────────────────────────────┘
        ▲  keys, paste, resize, scroll from commands
```

- One engine thread per pane. It is the only owner of the engine and the PTY writer.
- The PTY reader is a separate thread so a blocked `read` never stalls input.
- **Queues are bounded.** A fast producer must slow the reader down, not grow memory (today the channel is
  unbounded; fixing that is DESK-19).
- Frames are paced: at most one per 8 ms per pane, and none while nothing changed.
- Closing a pane drops the PTY master, which hangs the shell up. Today the reader thread keeps a sender alive and the
  shell survives; fixing that is DESK-14.
- Layout mutations go through one `Mutex<Layout>`; they are rare and short.

## 6. The app protocol

The webview talks to the core through a small, versioned set of commands and events.

**Commands** (webview → core), all addressed by `pane` where they concern one: `start_pane`, `input`, `key`,
`resize`, `scroll`, `split`, `new_tab`, `new_workspace`, `close_pane`, `focus_pane`, `cycle_focus`, `select_tab`,
`select_workspace`, `set_ratio`, `set_theme`. Layout commands return the new `View`.

**Events** (core → webview): `frame`, `layout`, `pane-exit`, `bell`, `title`.

**Frame protocol.** A frame is run-length encoded rows `[text, cols, fg, bg, flags]` with `\0` marking the spacer
cell of a wide character. Target format for DESK-11: a frame carries `full: bool` and only the rows that changed,
`rows: [(index, runs)]`; the webview keeps the previous frame per pane and patches it. A full frame is sent on
resize, theme change and when a webview (re)connects. A sequence number detects a lost frame and asks for a full one.

**Security of the protocol.** Commands are the attack surface of anything that runs in the webview.

- `withGlobalTauri` exposes the commands to every script in the page. The page loads only its own bundle under a
  strict CSP, so this is acceptable today; the target is to scope capabilities per window and to drop the global.
- `input` is able to write to any PTY. That is the point of the product, so the guard is the CSP and never
  rendering untrusted HTML in the page (previews run in a separate, sandboxed frame or window).

## 7. Data and persistence

| Data | Store | Synced | Notes |
|---|---|---|---|
| Layout (workspaces, tabs, tree) | `layout.json` ✅, atomic write | yes | repaired or reset when inconsistent (DESK-15) |
| Settings, themes | `settings.json` → SQLite | yes | |
| Connection profiles | SQLite | yes | secret fields hold a keychain key, never the secret |
| Command history | SQLite (FTS) | optional | fuzzy search, per host |
| Secrets | OS keychain | only inside the encrypted payload | |
| Scrollback | memory only | no | the engine owns it |

The sync payload and its encryption are specified before any sync code exists: see [sync.md](sync.md).

## 8. Rendering and input

**Rendering.** A pane is a grid; the core never draws. The webview paints all panes of a tab on one canvas:
`begin(bg)`, `drawPane(frame, rect)` per pane, with a scissor box per pane. WebGL2 uses a glyph atlas (R: coverage in
alpha, one slot per glyph and style, two adjacent slots for wide glyphs) and one instanced draw per pane. Canvas 2D
is the fallback when WebGL2 is missing. Both are covered by a pixel test that checks each pane stays inside its
rectangle.

**Input.** DOM key events are translated to an engine-neutral `KeyEvent { code, text, mods, action }` and sent to the
core, where the engine's key encoder produces the bytes for the current modes (application cursor keys, Kitty
keyboard flags, `modifyOtherKeys`). The frontend never builds escape sequences itself, apart from the minimal table
used until the encoder lands (DESK-09). Paste uses bracketed paste when the program asked for it (DESK-13).

**Replies.** Some sequences need an answer (device attributes, size reports). The engine collects them and the
engine thread writes them to the PTY before the next read (DESK-10).

## 9. Integrations and extensions

Integrations (Kubernetes, Talos, Azure, GAM7) are built in. Each implements `Integration`: detect the binary,
expose a context switcher, parse output into a typed model, and offer a block. They run the tool as a child process
on the active `Connection`, so they work on remote hosts too, and their credentials come from `SecretStore`.

User widgets are data, not code: a JSON document validated against a schema, rendered by the webview from a fixed
set of widget kinds. There is no arbitrary script execution.

## 10. Security model

| Asset | Threat | Control |
|---|---|---|
| Passwords, keys, tokens | read from disk | OS keychain only; profile files hold references |
| Terminal output | sent to an AI service unasked | context leaves the device only on explicit, per-request consent; local provider is the default |
| Synced settings | read by the server | end-to-end encryption, the server sees ciphertext and sizes |
| The PTY | input injected by page content | strict CSP, no untrusted HTML in the main page, previews sandboxed |
| Layout / settings files | corrupt or hostile file crashes the app | validated on load, fall back to a fresh state |
| Paste | control sequences run commands | bracketed paste; warn on multi-line paste to a shell that does not support it |

## 11. Platforms

| | macOS | Windows | Linux | iOS | Android |
|---|---|---|---|---|---|
| Webview | WKWebView | WebView2 | WebKitGTK | WKWebView | System WebView |
| Local shell | ✅ | ✅ (ConPTY) | ✅ | ❌ | ✅ |
| Keychain | Keychain | Credential Manager | Secret Service | Keychain | Keystore |
| Apple AI | ✅ 26+ | – | – | later | – |

Differences are confined to the shell crate and to implementations of the traits in section 4. The layout model and
the frame protocol are identical everywhere; mobile shows one block per screen from the same `View`.

## 12. Quality strategy

All of it runs locally; GitHub builds only on request.

- **Core:** unit tests per module (layout, engine adapter, PTY), property tests for the layout tree.
- **App shell:** persistence and command tests without a window.
- **Frontend:** pixel tests for the renderers in WebKit and Chromium, plus the renderer benchmark. A TypeScript test
  runner is added only if the logic outgrows these.
- **Performance budget:** parse ≥ 250 MiB/s, one frame ≤ 2 ms on a 160x50 grid, keystroke to screen p99 ≤ 5 ms.
  The spike measured 289 MiB/s, 0.6 to 1.1 ms and 291 µs.
- **Robustness:** corrupt `layout.json`, closing panes mid-output, and resizing during a flood are explicit tests.

## 13. Build and release

- Toolchain: Rust stable, Zig 0.16.0 for `libghostty-vt` (0.17 breaks Ghostty's `build.zig`), Node and `tsc` for
  the frontend. `libghostty-rs` is pinned by git revision.
- Version `0.0.1-alpha1`, then `0.0.1-beta1`, then `0.0.1`. Alpha and beta are GitHub prereleases.
- Local build and signing first; one GitHub build only when asked.

## 14. Decisions

| # | Decision | Reason |
|---|---|---|
| A1 | One Rust core, thin shells | one implementation for five platforms |
| A2 | Engine behind a trait | the engine API is unstable |
| A3 | Own WebGL2 renderer | 0.6 ms against 4.5 to 7.7 ms per frame |
| A4 | Engine stays native, no WASM | PTY, SSH and mobile need it anyway |
| A5 | Core owns state, webview mirrors | reload-safe, one source of truth, same on mobile |
| A6 | Tauri 3 alpha | the newest; the cost is API churn |
| A7 | No frontend framework or bundler | few dependencies; the UI is small |
| A8 | Integrations built in, widgets as data | no third-party code execution |

## 15. Open questions

1. Tauri 3 stability for iOS and Android, to be checked when phase 9 starts.
2. `opendal` against `reqwest_dav` for sync (size and TLS stack).
3. Whether the frame diff needs per-cell damage or per-row is enough.
4. SQLite crate choice (`rusqlite` against `sqlx`) with the dependency budget in mind.

See also: [design.md](design.md) for colour, type, components and the window direction.
