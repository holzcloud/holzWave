# Architecture

Status: **plan**, with the parts built in `0.0.1-alpha1` marked ✅. The layout of crates and the boundaries
between them are fixed; the contents of the later phases are a plan and will move. Sections 3, 5, 6, 7, 8 and 12
describe the desktop MVP as it was built in Phase 2 (macOS; Windows and Linux follow in Phase 2.2).

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
| `core/` | `holzwave-core` | everything above except the shell | ✅ engine, pty, pane, layout, settings, session |
| `app/src-tauri/` | `holzwave-app` | Tauri command layer, window, plugins | ✅ |
| `app/src/` | TypeScript | chrome, renderers, input | ✅ |
| `spike/` | `holzwave-spike` | measurement tool, not shipped | ✅ |
| `wsh/` | `holzwave-wsh` | CLI, depends on a small `holzwave-control` client crate | phase 6 |
| `plugins/apple-ai/` | Swift package | Foundation Models bridge | phase 7 |
| `docs/` | | roadmap, architecture, design, sync, config, test plan | ✅ |

`holzwave-core` stays free of Tauri and of any UI type. It must build and test on its own, and it is the crate the
mobile shells link.

The modules of `holzwave-core` as built (`core/src/`):

| Module | Role | State |
|---|---|---|
| `engine` | the `TerminalEngine` trait, the screen model and every engine-neutral type (keys, mouse, selection, search) | ✅ |
| `ghostty` (private) | the only file that sees `libghostty-vt` types (D-05); behind the `ghostty` feature | ✅ |
| `pty` | spawning a shell on a pseudo terminal, closing it, spawn options, working directory of a process | ✅ |
| `queue` | the bounded per-pane queue between the PTY reader and the engine thread | ✅ |
| `pane` | the pane actor: engine thread, PTY reader and writer, frame pacing, ending (D-19) | ✅ |
| `wire` | the frame protocol: engine update to frame message with changed rows and sequence numbers | ✅ |
| `layout` | workspaces, tabs, split tree, geometry, validation and repair of a loaded file | ✅ |
| `persist` | atomic writes, bounded reads, `.bad` copies, the debounced saver | ✅ |
| `settings` | `settings.json`: typed, versioned, validated key by key | ✅ |
| `theme` | built-in and user themes, terminal palette and chrome tokens, contrast helpers | ✅ |
| `keybindings` | action ids, chord grammar, default table, resolution of overrides | ✅ |
| `session` | `session.json`: per-pane working directory and window geometry, machine-local | ✅ |

The app shell keeps one more type of its own, `AppCore` (`app/src-tauri/src/state.rs`): it holds the layout, the
running panes, the settings and the session, has no Tauri type in it, and is tested headless with a recording sink
and real shells. `lib.rs` and `commands.rs` only adapt it to Tauri.

## 4. Core boundaries

Each boundary is a trait in `holzwave-core` with at least one real implementation and a test double.

| Trait | Methods (sketch) | Implementations | Why a boundary |
|---|---|---|---|
| `TerminalEngine` ✅ | `feed`, `resize`, `scroll`, `update` (dirty rows), `take_replies`, `encode_key`, `encode_text`, `paste`, `mouse`, `wheel`, `select`, `search`, `copy_mode`, `set_theme` | `GhosttyEngine` ✅, `AlacrittyEngine` | `libghostty-vt` is young and its API moved already between 0.2.2 and master |
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

- One engine thread per pane (`pane.rs`). It is the only owner of the engine and the PTY child. A PTY writer thread
  owns the write side behind an unbounded channel, so a blocking write can never stall the engine thread (a 4 MiB
  paste into `cat` would otherwise deadlock against the bounded queue).
- The PTY reader is a separate thread so a blocked `read` never stalls input.
- **Queues are bounded.** The output queue (`queue.rs`) holds at most 1 MiB of bytes plus one chunk; past that the
  reader blocks, the kernel's PTY buffer fills and the child blocks in its own `write`. Control messages (keys,
  resize, close) are unbounded, small and handed out before any output, so input is never stuck behind a flood.
  Replies to a program's own queries wait at most 256 KiB for the writer; past that whole replies are dropped, what
  the user typed or pasted never is.
- Frames are paced: at most one per 8 ms per pane, and none while nothing changed. The pace counts frames sent, not
  attempts, so a key that changes nothing does not delay the echo of the next one.
- **Closing is explicit and ends in one place.** A pane ends by `close` (or by dropping the last handle), by the shell
  exiting, or by a failed start; the engine thread closes the queue, kills and reaps the child, emits one `Exited`
  event and only then lets the PTY writer go. `close` never blocks the caller, and the reader keeps draining until the
  child is reaped (on macOS a child cannot finish exiting while its output is unread). The end of the output is
  queue state ordered after every queued chunk, so the last output of a short-lived shell is not lost.
- Layout mutations go through one `Mutex<Layout>`; they are rare and short. Writes of layout and session are
  debounced (250 ms and 2 s after the first change of a burst) and flushed on quit.

## 6. The app protocol

The webview talks to the core through a deny-by-default set of 31 commands and six events. The Rust types are the
source of truth; `app/tests/fixtures/protocol.json` is the golden written from them (`UPDATE_FIXTURES=1`) and read by
the TypeScript tests too, so a renamed argument or a changed payload fails on both sides at once. Arguments are
snake_case everywhere (`cell_w`, `allow_unsafe`).

**Commands** (webview → core), addressed by `pane` where they concern one:

| Group | Commands | Answer |
|---|---|---|
| Layout | `get_layout`, `split(column)`, `new_tab`, `new_workspace(name)`, `close_pane(pane)`, `focus_pane(pane)`, `cycle_focus(step)`, `select_tab(index)`, `select_workspace(index)`, `set_ratio(split, ratio)` | the new `View` |
| Pane lifecycle | `start_pane(pane, cols, rows, cell_w, cell_h)`, `resize(pane, cols, rows, cell_w, cell_h)` | nothing (`start_pane` waits for the shell to start) |
| Input | `key(pane, code, key, mods, repeat, release, composing)`, `text(pane, text)`, `paste(pane, text, allow_unsafe)`, `mouse(pane, action, button, x, y, mods)`, `wheel(pane, rows, x, y, mods)` | nothing; `paste` answers `written`, `empty` or `rejected` |
| Viewport and frames | `scroll(pane, rows)`, `scroll_to_bottom(pane)`, `request_full(pane)` | nothing |
| Selection, search, copy mode | `select(pane, op)`, `selection_text(pane)`, `search(pane, op)`, `copy_mode(pane, op)` | the text, `{total, current}`, `{active, selecting, yanked}` |
| Settings | `get_settings`, `set_setting(key, value)`, `reset_setting(key)`, `reload_settings` | the settings view |
| System | `app_info`, `open_url(url)`, `reveal_path(path, base)` | the version, OS and host name; nothing |

The `op` payloads are internally tagged with `kind` and snake_case names (`{kind: "begin", x, y, mode: "word"}`,
`{kind: "move", motion: "line_start"}`). The old `input` command (raw bytes to any PTY) is gone, and there is no
`set_theme`: the theme is the `theme` setting, changed with `set_setting`. Commands that wait for the engine thread's
answer (up to 2 s) or start a process are `async` and never run on the main thread; `mouse` and `wheel` are hot paths
that only bound their numbers and push to the pane's queue.

**Events** (core → webview), emitted to every webview as JSON:

| Event | Payload |
|---|---|
| `frame` | a frame message of one pane (below) |
| `layout` | the `View`: workspaces, tabs of the active workspace, panes and dividers of the active tab in unit rectangles |
| `title` | `{pane, title}` from OSC 0 and 2, cut to 4096 bytes without control characters |
| `cwd` | `{pane, cwd}` from OSC 7, absolute paths only |
| `pane-exit` | `{pane, code}`, `code` is `null` when the exit status is unknown |
| `settings` | the settings view, after a command or a hand edit that was picked up |

The settings view is `{settings, themes, theme, keybindings, warnings}`: every setting, the theme list, the theme in
force with all its colours, the resolved key binding rows (`action`, `title`, `chord`, `is_default`) and plain
sentences about whatever could not be used. The page decides nothing about validity.

**Frame protocol.** One JSON object per pane and change:

```
{ pane, seq, full, cols, rows, fg, bg,            // fg and bg: default colours as 0xRRGGBB
  cursor,                                         // {x, y, style: block|bar|underline|hollow, blink} or null
  scrollbar,                                      // [offset, total, len]
  selection, matches, search, copy_cursor, modes, // spans [y,x0,x1], matches [y,x0,x1,current], {total,current},
                                                  // [x,y], {alt, bracketed_paste, mouse}
  changed }                                       // the rows that differ from the previous frame
```

A row is `{y, runs, extras?}`. A run is `[text, cols, fg, bg, flags]` with flags bold 1, italic 2, underline 4,
inverse 8. **Every cell contributes exactly one code point to its run's text**: the first code point of its grapheme
cluster, a space for an empty cell, `\0` for the second column of a wide character. The rest of a cluster (a combining
mark, the tail of a ZWJ sequence, a skin tone) travels in the row's `extras` as `[column, tail]`. So
`text.chars().count() == cols` for every run and a renderer advances one cell per code point without measuring text.
Trailing blanks in the default style are not sent; the consumer pads to `cols`.

- `changed` holds only rows whose encoding differs from the last frame sent. The engine reports a full redraw for every
  scroll; the encoder compares rows and turns that back into the rows that really changed (D-11).
- `full` is true only on the first frame, after a size change, after a theme change and after `request_full`. The
  webview then replaces its mirror of the pane, `changed` carrying every row.
- `seq` starts at 1 per pane and goes up by exactly one per emitted frame. A frame with no changed row and the same
  header is not emitted and uses no number; a header change alone (cursor, selection, matches) is a frame.
- **The `request_full` rule.** The page asks for a full frame in two cases: after `start_pane` (a webview that
  reloaded attaches to a pane that is already running and may stay idle), and on a gap, that is a non-full frame
  whose `seq` is not the mirror's plus one or whose size differs. It then ignores the pane's non-full frames until the
  full one arrives, and never patches over a gap.
- Frames are events; D-16 keeps that and measures it (section 12).

The golden sequence `app/tests/fixtures/frames.json` is written by the `fixtures` test of `core/tests/wire.rs` and
applied step by step by the TypeScript patcher, so both languages are proven against the same bytes.

**Security of the protocol.** Commands are the attack surface of anything that runs in the webview.

- **Deny by default.** The app manifest in `build.rs` lists every command; a command is callable only when the
  capability `capabilities/default.json` grants its `allow-<command>` permission, and only to the window `main`. The
  invoke handler of `lib.rs`, the manifest and the capability must list the same commands, and
  `app/src-tauri/tests/acl.rs` fails when they do not. Adding a command means changing all three and regenerating the
  protocol golden.
- **No raw input.** The page sends facts (`key`, `text`, `paste`); the engine's encoder produces the bytes. A
  command that can write to any PTY is still the point of the product, so the guard is the strict CSP and never
  rendering untrusted HTML in the page (previews run in a separate, sandboxed frame or window).
- **Strict CSP.** `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' holzfile:
  http://holzfile.localhost; media-src 'self' holzfile: http://holzfile.localhost; font-src 'self'; connect-src ipc:
  http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'`, pinned by `acl.rs`. No `blob:`, no
  `data:`, no inline script or style. The page uses no native `<select>` and never `innerHTML`; text from the core goes
  in with `textContent`. A Markdown document is the one piece of HTML: the core cleans it with an allow-list sanitiser
  (`ammonia`), and the page parses it into an inert document and copies it node by node through an allow-list of its own
  (`safehtml.ts`), so a script, a handler, a style, an image or a `javascript:` link cannot arrive even if one of the two
  lists had a hole.
- `withGlobalTauri` is still on, so the commands are reachable from every script of the page. The page loads only its
  own bundle under the CSP above and the capability is scoped to the one window `main`; dropping the global is still
  the target.
- **Everything is bounded in the core**: pane sizes, pointer numbers and wheel rows are clamped, text input is limited
  to 1 MiB and a paste to 16 MiB, key fields to 256 bytes, links to 2048 bytes, titles and working directories to
  4096 bytes. A link is opened only for `http`, `https` and `mailto`, through the platform opener started with
  an argument list (never a shell), and a path is revealed in the file manager and never run.

## 7. Data and persistence

| Data | Store | Synced | Notes |
|---|---|---|---|
| Layout (workspaces, tabs, tree) | `layout.json` ✅, atomic write | yes | repaired or reset when inconsistent (DESK-15) ✅ |
| Settings, key binding overrides | `settings.json` ✅ → SQLite later | yes | read key by key, one bad value costs one key; formats in [config.md](config.md) |
| Themes | `themes/*.json` ✅ | yes | one file per theme, validated; `#rrggbb` colours only |
| Working directories, window frame | `session.json` ✅ | no | machine-local hints; a damaged file costs the hint, never the start |
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

**Input.** The page sends facts, never bytes: `key` carries the physical key (`code`), the typed key (`key`), the
modifier bits (shift 1, alt 2, ctrl 4, super 8) and `repeat`, `release` and `composing`. The core turns them into an
engine-neutral `KeyEvent` and the engine's key encoder produces the bytes for the current modes (application cursor
keys, Kitty keyboard flags, `modifyOtherKeys`). The frontend builds no escape sequence (DESK-09).

- Option is not Alt by default (D-12, safe on a Swiss layout); `option_as_alt` is a setting that is pushed to live
  panes. Ctrl+`[`, Ctrl+I and Ctrl+M take the engine's behaviour (D-13).
- A hidden textarea keeps the keyboard focus so that input methods and dead keys work; committed text arrives as
  `text`, which is not a paste and is never bracketed.
- Shortcuts are chords in one canonical spelling (`Cmd+Shift+D`). The core resolves defaults and the user's overrides
  into one table and sends its rows with the settings view; the page looks a pressed chord up there and runs the
  action from its registry, so the command palette lists the same actions. A key that ran no action goes to the
  terminal.
- **Mouse and wheel** are sent as device pixels from the grid origin with the modifier bits, and the engine decides
  what they mean (D-24). A program that tracks the mouse gets reports in the encoding it asked for; Shift held keeps
  selecting text. The wheel policy, in order: Shift scrolls the viewport; a mouse-tracking program gets wheel
  reports; the alternate screen with alternate scroll gets arrow keys (application cursor mode honoured); anything
  else scrolls the viewport.
- **Selection, search and copy mode live in the core.** The page sends gestures (`select`, `search`, `copy_mode`) and
  draws what the frame header says (spans, matches, the copy cursor); copy reads the text back with
  `selection_text`.

**Replies.** Some sequences need an answer (device attributes, size reports, colour queries). The engine collects
them and the engine thread hands them to the PTY writer thread; a program that waits for an answer does not hang
(DESK-10). Colour queries are answered with the theme's colours and end with the terminator of the query. A reply
over 64 KiB is refused as a whole, never cut, and replies are dropped when the writer is 256 KiB behind.

**Paste.** `paste(pane, text, allow_unsafe)`: the engine always sanitises, brackets the text when the program turned
on bracketed paste (DESK-13), and answers `rejected` for text that could inject commands (a newline without bracketed
paste, or the end marker with it) without writing anything. The engine never decides to allow it. The page then asks
the person (`confirm_multiline_paste`, D-15) and, on yes, sends the same text again with `allow_unsafe: true`.

## 9. Integrations and extensions

Integrations (Kubernetes, Talos, Azure, GAM7) are built in. Each implements `Integration`: detect the binary,
expose a context switcher, parse output into a typed model, and offer a block. They run the tool as a child process
on the active `Connection`, so they work on remote hosts too, and their credentials come from `SecretStore`.

User widgets are data, not code: a JSON document validated against a schema, rendered by the webview from a fixed
set of widget kinds. There is no arbitrary script execution.

## 9a. Files and previews (Phase 4)

A **block pane** is a pane that runs no process: its link is a `NullLink` and the page draws its content (a file browser
or the preview of one file) over the empty terminal. It has a header, a place in the split tree, drag and close like any
pane, and `pane_blocks.json` remembers what each shows. `core/src/files.rs` has one `FileSource` interface with two
implementations: this computer, and the files of a saved SSH connection over SFTP on a connection of its own (a file read
never waits behind a terminal's output). Reads are bounded, writes are atomic (written aside, renamed over).

Images, video, audio and PDFs are **not** loaded into the page. `file_grant` makes a token (128 random bits) for one file,
and the `holzfile` URI scheme serves exactly that file from `holzfile://localhost/<token>` (`http://holzfile.localhost/`
on Windows and Android) with byte ranges, so a video starts and seeks at once and a large file costs no memory. There is no
path in the URL, so the page cannot ask for a file it was not given; a grant ends when its preview goes
(`file_revoke`), at most 256 live at once. Responses say `nosniff`, `no-store` and `Content-Security-Policy: sandbox`.

## 10. Security model

| Asset | Threat | Control |
|---|---|---|
| Passwords, keys, tokens | read from disk | OS keychain only; profile files hold references |
| Terminal output | sent to an AI service unasked | context leaves the device only on explicit, per-request consent; local provider is the default |
| Synced settings | read by the server | end-to-end encryption, the server sees ciphertext and sizes |
| The PTY | input injected by page content | strict CSP, no untrusted HTML in the main page, previews sandboxed |
| Layout / settings files | corrupt or hostile file crashes the app | validated on load, fall back to a fresh state |
| Paste | control sequences run commands | bracketed paste; warn on multi-line paste to a shell that does not support it |
| Everything the app is | telemetry, an update channel or an account arrives through a dependency or a plugin (DESK-26) | none exists; `core/tests/project_policy.rs` pins the dependency lists and the closure of the app, scans first-party sources and the page for network APIs and fails on a CI workflow |

## 11. Platforms

| | macOS | Windows | Linux | iOS | Android |
|---|---|---|---|---|---|
| Webview | WKWebView | WebView2 | WebKitGTK | WKWebView | System WebView |
| Local shell | ✅ | written (ConPTY), not yet run | written, not yet run | ❌ | ✅ |
| Keychain | Keychain | Credential Manager | Secret Service | Keychain | Keystore |
| Apple AI | ✅ 26+ | – | – | later | – |

Differences are confined to the shell crate and to implementations of the traits in section 4. The layout model and
the frame protocol are identical everywhere; mobile shows one block per screen from the same `View`.

### The platform seam (Phase 2.2)

Windows and Linux are written and cross-checked from a Mac; **they have not been run on their own platform**, and the
owner's run of [test-plan.md](test-plan.md) Part 4 decides whether they work. What exists, as built:

- **One `Os` value, pure functions.** `core/src/platform.rs` holds every per-OS decision of the core as a function of an
  `Os` and injected facts (environment, an `exists` closure), so each platform's rule is tested on any host: the default
  shell and its arguments (`shell_launch`, `resolve_shell`), the UTF-8 locale, the exit-poll interval and the rename
  retries. `child_env_on` does the same for the child's environment.
- **The shell.** A new pane starts the program of the `shell` setting, else the platform default (macOS and Linux:
  `$SHELL`; Windows: PowerShell 7, Windows PowerShell, `%ComSpec%`, `cmd.exe`). A setting that cannot be started falls back
  to the default with one visible sentence. The app crate asks per pane start (`ShellSpec` in `state.rs`).
- **Windows panes.** `core/src/winshell.rs` holds the constant PowerShell prompt wrapper (passed as base64
  `-EncodedCommand`) that announces the directory with OSC 7, and the reader that accepts such a report only as a drive
  path of this machine. A pane ends when its shell does even if the console host never closes the output (`PaneSpec.exit_poll`,
  `try_wait` polled beside the reader). Settings files replace with a bounded retry on a sharing violation
  (`persist::retry_on_sharing_violation`). `cmd.exe` has no directory report.
- **Keys.** `Keybindings::default_for(os)` and the chord tables of `docs/config.md` per platform; AltGr is recognised
  by the page and backed up by the core on Windows (`KeyEvent::from_dom_on`); the clipboard chords are fixed per OS.
- **The shell of the window.** `app/src-tauri/src/state.rs` builds the platform launchers (`open_launch`,
  `reveal_launch`, with the Windows refusal list for UNC and device paths), the host name and home directory;
  `windows.rs` places new windows in the pointer's unit space at any display scale and knows that under Wayland the
  pointer and the window position are unknown; `textscale.rs` reads the OS text size (Windows `reg.exe`, GNOME
  `gsettings`) behind the `os_text_scale` command.
- **Packaging.** `tauri.windows.conf.json` (NSIS only, per user, unsigned), `tauri.linux.conf.json` (deb, rpm, AppImage)
  and the desktop template `app/src-tauri/linux/holzwave.desktop`; `app/src-tauri/tests/bundle.rs` pins them. The build
  guides are [build-windows.md](build-windows.md) and [build-linux.md](build-linux.md); the deviations from macOS are in
  [design.md](design.md) section 7.
- **The cross-check ladder, from the Mac.** `scripts/cross-check.sh` compiles every test target of the core for
  `x86_64-pc-windows-gnu` and `x86_64-unknown-linux-gnu` and the app for Windows (GNU ABI: Zig cannot build the MSVC
  library on a Mac, so the MSVC build exists only on a Windows host). `scripts/linux-container-test.sh` runs the core and
  app tests in an Ubuntu container, and `scripts/linux-container-bundle.sh` builds and inspects the deb and rpm there.
  They prove that it compiles and that the Unix half passes on Linux; they do not prove a screen, an input method, a GPU
  or ConPTY.

## 12. Quality strategy

All of it runs locally; GitHub builds only on request.

- **Core:** unit tests per module and integration tests in `core/tests/` (engine, keys, mouse, replies, selection,
  search, panes, queue, PTY, settings, themes, session, persistence, wire). They share helpers in `core/tests/common`:
  scratch directories, `wait_until`, an engine factory and a seeded xorshift generator, so a failing invariant test
  prints a seed that replays it. Pane and PTY tests start real shells.
- **Golden fixtures:** the frame sequence (`app/tests/fixtures/frames.json`, written by `core/tests/wire.rs`) and the
  command and payload table (`app/tests/fixtures/protocol.json`, written by the app shell's `protocol` test module)
  are generated from the Rust types with `UPDATE_FIXTURES=1` and read by the TypeScript tests, which also scan every
  `call(...)` of the page against the command table.
- **App shell:** `AppCore` is tested headless with a recording sink and real shells: layout commands, persistence,
  settings, session, the opener, shutdown. `app/src-tauri/tests/acl.rs` keeps the invoke handler, the manifest and the
  capability in step.
- **Frontend:** unit tests (`npm test`, `node:test` over the compiled modules) and the browser harness
  (`npm run test:browser`): Playwright 1.64 drives WebKit and Chromium against `app/dist` with an in-page Tauri stub,
  with real pixel readback for the renderers, and fails a test on any page error or CSP violation. The renderer
  benchmark is built apart from the bundle (`npm run bench:build`).
- **Policy:** `core/tests/project_policy.rs` makes the negative requirements executable (DESK-26, D-03, D-04).
- **Performance budget:** parse ≥ 250 MiB/s, one frame ≤ 2 ms on a 160x50 grid, keystroke to screen p99 ≤ 5 ms.
  The spike measured 289 MiB/s, 0.6 to 1.1 ms and 291 µs. The budget tests of the app are `#[ignore]` release tests
  (`cargo test --release -p holzwave-core --offline --test budget -- --ignored --nocapture`). Measured on an Apple
  M1 Max, macOS, `release` profile (thin LTO), on 2026-10-11:

  | Budget | Limit | Measured |
  |---|---|---|
  | engine update plus wire encode, forced-full coloured 160x50 frame, 200 frames | average ≤ 2 ms | average 0.47 ms, p99 0.56 ms, max 1.24 ms |
  | key to frame through the pane actor with `cat`, 200 samples, at the sink | p99 ≤ 5 ms | p50 0.15 ms, p99 0.86 ms, max 1.03 ms |

  Both are measured without the webview: the event path and the WebGL draw come on top, and are read from the status
  line (draw time and fps) during the human checklist in [test-plan.md](test-plan.md). The parse rate was not
  measured again in Phase 2.
- **Robustness:** corrupt `layout.json`, closing panes mid-output, and resizing during a flood are explicit tests,
  as are hostile `settings.json`, `session.json` and theme files (size, type, range and name limits).
- **What no suite here can see** (real window chrome, an input method, a physical layout, the native menu, fonts, the
  process table after quit) is one human checklist, [test-plan.md](test-plan.md), with a per-platform list for
  Windows and Linux (D-22).

## 13. Build and release

- Toolchain: Rust stable, Zig 0.16.0 for `libghostty-vt` (0.17 breaks Ghostty's `build.zig`), Node and `tsc` for
  the frontend. `libghostty-rs` is pinned by git revision.
- Version `0.0.1-alpha1`, then `0.0.1-beta1`, then `0.0.1`. Alpha and beta are GitHub prereleases.
- Local build and signing first; one GitHub build only when asked.
- Windows builds an unsigned per-user NSIS installer, Linux a deb, an rpm and an AppImage, each on the owner's own machine
  ([build-windows.md](build-windows.md), [build-linux.md](build-linux.md)); Zig is exactly 0.16.0 and `scripts/env.sh` and
  `scripts/env.ps1` find it. No guide builds on GitHub, and no tag or release is made by them.

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
| A9 | Frames are events with changed rows only, a sequence number and an explicit full frame (D-09, D-11, D-16) | measured: 0.47 ms to encode a full 160x50 frame; the webview mirrors and patches |
| A10 | Commands are deny by default; the page sends key facts, never bytes | a webview script gets only the listed commands, and no raw write to a PTY |

## 15. Open questions

1. Tauri 3 stability for iOS and Android, to be checked when phase 9 starts.
2. `opendal` against `reqwest_dav` for sync (size and TLS stack).
3. ~~Whether the frame diff needs per-cell damage or per-row is enough.~~ Answered in Phase 2: per row, compared
   against the rows last sent (section 6); a full-frame encode costs 0.47 ms on 160x50.
4. SQLite crate choice (`rusqlite` against `sqlx`) with the dependency budget in mind.
5. Tauri 3 alpha links `reqwest` unconditionally, for a dev-server proxy that a desktop build never creates. The policy
   test names it as an exception with its exact parent (`tauri`); when a later Tauri 3 release makes it optional, the
   exception goes.

See also: [design.md](design.md) for colour, type, components and the window direction.
