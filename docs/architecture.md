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

**Rendering is a spike decision.** Ghostty's engine compiled to WASM inside the webview keeps one code path across
platforms; an own WebGL renderer gives more control. The spike measures throughput, latency and memory for both and
records the result here.

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
