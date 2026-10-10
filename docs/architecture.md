# Architecture

This is the intended shape, to be confirmed or changed by the phase 1 spike.

```
┌──────────────────────────── app shell (Tauri 2) ───────────────────────────┐
│  UI: TypeScript (React or Solid)                                           │
│  terminal block: ghostty WASM in the webview  —or—  own WebGL renderer     │
└───────────────▲──────────────────────────────────────────────▲─────────────┘
                │ commands / events                            │ plugin API
┌───────────────┴────────────── core (Rust crate) ─────────────┴─────────────┐
│  TerminalEngine trait ── libghostty-vt │ alacritty_terminal (fallback)     │
│  PTY: portable-pty     SSH: russh      Store: SQLite                       │
│  Connections (local · SSH · WSL · Docker) ── used by terminal AND previews │
│  Sync (WebDAV, E2E-encrypted)   Secrets (OS keychain)   AI providers       │
└───────────────▲──────────────────────────────────────────────────────────┘
                │ Unix socket / named pipe
             wsh (Rust binary)
```

## Decisions and why

**One core, many shells.** Terminal logic, connections, sync and storage live in one Rust crate. The Tauri shell
for desktop, iOS and Android is thin, so a feature is written once.

**The engine sits behind a `TerminalEngine` trait.** `libghostty-vt` is young and its API may move. The rest of the
code talks to the trait (feed bytes, read the screen grid, resize, scrollback, selection), so swapping to
`alacritty_terminal` is a change in one crate.

**Rendering: own WebGL2 renderer, Canvas 2D as fallback (decided in the phase 1 spike).** The engine stays in the
Rust core and sends the webview a run-length encoded frame; the frontend draws it. A WebGL2 renderer with a glyph
atlas and one instanced draw per frame was measured against a plain Canvas 2D renderer on a 160x50 grid, 300 frames
with a GPU sync after each:

| Engine | Canvas 2D | WebGL2 |
|---|---|---|
| WebKit (WKWebView on macOS, what Tauri uses there) | 6.8 - 7.7 ms | 0.6 ms |
| Chromium (WebView2 on Windows) with ANGLE/Metal | 4.6 - 6.0 ms | 0.64 ms |

Timer resolution in WebKit is 1 ms, so those numbers are coarse. Compiling Ghostty's engine to WASM for the webview
was not pursued: the core needs the native engine anyway (PTY, SSH, mobile), and a second engine in the webview would
duplicate state. The benchmark is `app/src/bench.ts`.

**One connection abstraction.** A "connection" (local, SSH, WSL, Docker) offers a shell *and* file access. The
terminal block and the file previews both use it, so an editor on a remote file does not open a second login.

**Secrets never leave the keychain in clear text.** The secret store uses the OS keychain. When secrets are synced,
they travel only inside the encrypted sync payload.

**AI context is opt-in per use.** The chat sees terminal output only when the user allows it for that request.

## Layout of the repository (planned)

| Path | Contents |
|---|---|
| `core/` | the Rust crate |
| `wsh/` | the CLI binary |
| `app/` | the Tauri shell and the TypeScript UI |
| `plugins/` | the Swift bridge for Apple AI, other native plugins |
| `docs/` | roadmap, architecture, sync format, spike results |
