# Architecture

Status: **plan**, with the parts built marked ✅. The layout of crates and the boundaries between them are fixed; the
contents of the later phases are a plan and will move. Sections 3, 5, 6, 7, 8 and 12 describe the desktop MVP as it was
built in Phase 2 (macOS; Windows and Linux follow in Phase 2.2). The AI chat, the Apple on-device model and the extras
of Phase 7 are built and tested against in-process servers, scripted providers and a page stub; they have not been run
with a real provider account, a real Apple model or on Windows and Linux hardware, and the rows of Part 7 of
[test-plan.md](test-plan.md) are the owner's run.

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
              │  command layer · window · menus · capability scoping · keychain · Apple bridge crate   │
              └────────────▲──────────────────────────────────────────────────────────────────────────┘
                           │ library calls
 ┌─────────────────────────┴────────────────────── holzwave-core (Rust) ────────────────────────────────┐
 │  layout      workspaces · tabs · split tree · geometry                                  ✅             │
 │  engine      TerminalEngine trait ── libghostty-vt │ alacritty_terminal (fallback)      ✅ (ghostty)  │
 │  pty         local shell over portable-pty                                              ✅             │
 │  conn        Connection trait: local · ssh (russh) · wsl · docker                       built          │
 │  secrets     SecretStore trait over the OS keychain                                     built          │
 │  store       SQLite: settings, history, profiles                                        phase 2/7      │
 │  sync        encrypted WebDAV sync, conflict handling                                   built          │
 │  preview     file browser, markdown, media, editor backends                             built          │
 │  control     socket server for wsh                                                      built          │
 │  ai          Provider trait: HttpProvider on five wires · Apple bridge · scripted       built          │
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
| `core/` | `holzwave-core` | everything above except the shell | ✅ engine, pty, pane, layout, settings, session, connections, files, sync, the AI chat |
| `app/src-tauri/` | `holzwave-app` | Tauri command layer, window, plugins | ✅ |
| `app/src/` | TypeScript | chrome, renderers, input | ✅ |
| `spike/` | `holzwave-spike` | measurement tool, not shipped | ✅ |
| `wsh/` | `wsh` | CLI (`view`, `edit`, `setmeta`, `ping`, `hist`, `init`, `ai`); depends on the small `holzwave-ipc` crate (`ipc/`: protocol, socket server and client, streamed replies) | ✅ |
| `plugins/apple-ai/` | `holzwave-apple-ai` | Foundation Models bridge: a Rust crate with a Swift package behind a C interface, macOS only, linked weakly, not a Tauri plugin | ✅ (the real model not yet run) |
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
| `persist` | atomic writes, private (owner-only) atomic writes, bounded reads, `.bad` copies, the debounced saver | ✅ |
| `settings` | `settings.json`: typed, versioned, validated key by key | ✅ |
| `theme` | built-in and user themes, terminal palette and chrome tokens, contrast helpers | ✅ |
| `keybindings` | action ids, chord grammar, default table, resolution of overrides | ✅ |
| `session` | `session.json`: per-pane working directory and window geometry, machine-local | ✅ |
| `ai` | the AI chat core (`core/src/ai/`, below): the `Provider` trait and `HttpProvider` over five wire adapters in `ai/wire/` (`anthropic.rs`, `gemini.rs`, `ollama.rs`, `openai_chat.rs`, `openai_responses.rs`), the one HTTP chokepoint `ai/http.rs`, the context gate `gate.rs` and `redact.rs`, tools and approval in `tools/`, the agent loop `agent.rs`, the conversation store `store.rs`, profiles and keys `profiles.rs`, the service facade `service.rs` with `service/` | ✅ |
| `textread` | reads the screen, the selection, the last rows or the last command's output as text, with the cells nobody could read marked | ✅ |
| `exec` | runs one command in a separate process with a scrubbed environment, a time limit and a kill of everything it started | ✅ |
| `ssh` | the SSH connection, and now also the exec primitive: one command on a saved connection without a terminal, over a cached connection | ✅ |
| `connections` | the saved SSH, WSL and Docker connections, their secrets, and which pane runs which connection | ✅ |
| `blocks` | panes that are not a terminal: a file browser, a preview, the system monitor and a chat | ✅ |
| `monitor` | the system monitor of this computer (`sysinfo`) and, in `monitor/remote.rs`, of a Linux host through the exec primitive | ✅ |
| `widgets` | the sidebar widgets: data files checked field by field, described by `docs/widget.schema.json` | ✅ |
| `history` | the command history in SQLite, a fuzzy matcher, each use stored with the host it ran on | ✅ |

The AI chat in the shell and the page (`holzwave-core` itself stays free of Tauri and of any PTY code):

| Where | What |
|---|---|
| `app/src-tauri/src/ai_host.rs` | the host: `AppCore` holds one service, answers the `ai_*` commands, supplies the context source and the tool host, and registers the Apple provider only where it is available |
| `ai_tools.rs`, `ai_write.rs`, `ai_exec.rs` | the shell side of the agent's tools: file reads, writes with a diff and an undo log, commands in a separate process or through the exec session of a saved connection |
| `ai_ask.rs`, `ai_device.rs` | the confirmation of `wsh ai` requests, and the one question the shell asks of the Apple model (`DeviceSource`) |
| `wsh/src/ai.rs`, `ipc/src/stream.rs` | `wsh ai` and the streamed reply of the local socket |
| `plugins/apple-ai/` | `src/{backend,native,prompt,provider,pump,scripted}.rs` over `swift/Sources/AppleAI/` |
| `app/src/` | the chat (`chat`, `chatview`, `chatstate`, `chatlist`), the sidebar form (`aisidebar`), the providers dialog (`aisettings`, `aistate`, `ondevice`), consent (`consent*`), approval and undo (`approval*`, `undo`), `wsh ai` requests (`ask*`), the palette actions (`inline*`), and the monitor and history views (`monitorstate`, `historystate`, `historyview`) |

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
| `Provider` ✅ | `id`, `capabilities(model)`, `stream(request, callback, cancel) → completion` (blocking, with a cancel token) | `HttpProvider` ✅ over five wire adapters (local servers, cloud APIs, gateways), `AppleProvider` ✅ (Foundation Models), `ScriptedProvider` (the test double) | consent and context rules live above it: a provider accepts only context the gate approved |
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
- The pane actor also answers `read_text` on its engine thread (`TerminalEngine::read_text`, `PaneHandle::read_text`): the
  text of the screen, the selection, the last rows or the last command's output, bounded in bytes, rows and cells
  (`MAX_READ_BYTES`, `MAX_READ_ROWS`, `MAX_READ_CELLS` of `textread.rs`), with a wait of at most two seconds for the caller,
  and without touching the selection, the scroll position or the next frame.
- A chat turn runs on a turn thread of its own, never on a Tauri command thread: at most four run at once (one per running
  chat), so no command blocks a shell thread, and the streamed text is merged into about sixty events a second. A turn
  ends within a quarter second of the user's Stop, the HTTP call included.
- The exec primitive (`SshExec`) runs on one worker runtime of its own, a single worker thread named `hw-exec`, with a cached
  connection and no terminal; its callers only wait. The local command runner (`exec.rs`) uses plain threads for its two
  pipes and is polled, not blocked, for the end, the time limit and the stop.

## 6. The app protocol

The webview talks to the core through a deny-by-default set of 95 commands and 12 events. The tables below list the
commands of the terminal core and of the AI chat; the golden lists every command. The Rust types are the
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
| AI conversations | `ai_conversation_new`, `ai_conversation_list`, `ai_conversation_delete(conversation)`, `ai_send(conversation, text, pane, profile, model, offer, picked)`, `ai_cancel(conversation)`, `ai_turn_snapshot(conversation)` | an id (no file exists until the first send); the stored chats with a note for each file that cannot be used; whether the chat was removed; a ticket for the turn (`ai_send` answers at once and the reply arrives as `ai` events); whether a turn was stopped; the whole state of one chat |
| AI providers and keys | `ai_profiles`, `ai_profile_save(input)`, `ai_profile_remove(profile)`, `ai_key_set(profile, slot, api_key)`, `ai_key_clear(profile, slot)`, `ai_has_key(profile, slot)` | the profile views with the state of the on-device model; the id of the saved profile; whether the profile and its key are gone; nothing; nothing; whether a key is set. A key goes in and never comes out |
| AI context | `ai_context_state(conversation, pane, profile, model)`, `ai_context_share(conversation, pane, profile, model, shared)`, `ai_context_preview(conversation, pane, profile, model, kinds)` | what the pane can offer and whether it is shared (twice); an offer: labels, sizes and findings, never the text |
| AI decisions | `ai_decide(conversation, call_id, decision, hash)`, `ai_undo(conversation, call_id)` | nothing; webview commands only |
| `wsh ai` requests | `ai_ask_pending`, `ai_ask_answer(ask, verdict, seal)` | the requests that wait, nothing |
| Markdown | `markdown_render(text)` | the cleaned HTML of a reply |

Every refusal of the AI commands is an object with `code`, `reason` and `text` (the page words its own sentence from the
code and the reason and shows the text only as the core's plain sentence); a plain string is the shape of the older commands.

The `op` payloads are internally tagged with `kind` and snake_case names (`{kind: "begin", x, y, mode: "word"}`,
`{kind: "move", motion: "line_start"}`). The old `input` command (raw bytes to any PTY) is gone, and there is no
`set_theme`: the theme is the `theme` setting, changed with `set_setting`. Commands that wait for the engine thread's
answer (up to 2 s) or start a process are `async` and never run on the main thread; `mouse` and `wheel` are hot paths
that only bound their numbers and push to the pane's queue.

**Events** (core → webview), emitted to every webview as JSON unless the row says otherwise:

| Event | Payload |
|---|---|
| `frame` | a frame message of one pane (below) |
| `layout` | the `View`: workspaces, tabs of the active workspace, panes and dividers of the active tab in unit rectangles |
| `title` | `{pane, title}` from OSC 0 and 2, cut to 4096 bytes without control characters |
| `cwd` | `{pane, cwd}` from OSC 7, absolute paths only |
| `pane-exit` | `{pane, code}`, `code` is `null` when the exit status is unknown |
| `settings` | the settings view, after a command or a hand edit that was picked up |
| `incoming-drop` | `{kind, pane, x, y}`: a pane or tab released over this window comes from another window; the page answers with `attach_pane` or `attach_tab`, or with nothing |
| `drag-hover` | `{kind, pane, x, y}`: a pane or tab being dragged out of another window is over this one; the page draws the outline of the zone it would land in |
| `drag-end` | `null`: that drag left this window or ended; the page takes the outline away |
| `ai` | one event of a chat turn, to every window (the page routes it by `conversation`): `conversation`, `turn`, `seq` (from 1 per turn), `kind` (`turn_started`, `delta`, `reasoning`, `usage`, `trimmed`, `message`, `tool_proposed`, `tool_result`, `turn_ended`) and the fields of the kind |
| `ai-ask` | `{kind: waiting, view}` to the window of the pane that asked, `{kind: closed}` to every window: a `wsh ai` request waits for a person, or is over |
| `history-open` | `{pane}`: the shell of that pane asked for the history search (the `^R` binding); the window that has the pane opens the dialog |

**AI events.** A chat turn is a stream of `ai` events with a sequence number that starts at 1 in each turn and goes up by
one. The webview can reload at any time, so the stream is not a channel: a page that sees a gap, opens a chat or comes back
from a reload asks `ai_turn_snapshot` and goes on from the state it gets; a state that is behind the events is never
patched. Streamed text is merged to about sixty events a second, and the page never has to trust that it saw every event.
The golden `app/tests/fixtures/ai_stream.json` is a scripted run with the state after each event, read by the page's reducer.

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
- **Decisions are webview commands only.** Approval of a tool call, undo of a file change and the confirmation of a `wsh ai`
  request exist only as the commands `ai_decide`, `ai_undo` and `ai_ask_answer`; the local socket of `wsh` has no variant
  that can decide, approve, confirm or grant (a test reads the source for such words), and no command returns a key. The
  reviewed list `AI_COMMANDS` in `app/src-tauri/tests/acl.rs` names every AI command and is changed on purpose.
- **No raw input.** The page sends facts (`key`, `text`, `paste`); the engine's encoder produces the bytes. A
  command that can write to any PTY is still the point of the product, so the guard is the strict CSP and never
  rendering untrusted HTML in the page (previews run in a separate, sandboxed frame or window).
- **Strict CSP.** `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' holzfile:
  http://holzfile.localhost; media-src 'self' holzfile: http://holzfile.localhost; font-src 'self'; connect-src 'self'
  ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'`, pinned by `acl.rs`. No `blob:`, no
  `data:`, no inline script or style. The page uses no native `<select>` and never `innerHTML`; text from the core goes
  in with `textContent`. A Markdown document is the one piece of HTML: the core cleans it with an allow-list sanitiser
  (`ammonia`), and the page parses it into an inert document and copies it node by node through an allow-list of its own
  (`safehtml.ts`), so a script, a handler, a style, an image or a `javascript:` link cannot arrive even if one of the two
  lists had a hole. The copy is bounded in depth (64 levels), in nodes (200 000) and in the nodes of one block (5 000),
  because the layout is the expensive part: WebKit lays out a long run of inline boxes in time that grows with the square
  of their number (100 000 spans in one paragraph took nine seconds).
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
| Command history | `history.db` ✅, SQLite, owner-only, deleted rows overwritten | no | fuzzy search (`nucleo-matcher`); each use carries the label of the host it ran on (`local`, `ssh:{id}`, `wsl:{id}`, `docker:{id}`). Only commands typed in a pane of this computer or of a saved connection reach it, nothing is recorded from a far shell, and deletions are not synced, so a forgotten command would come back |
| Widgets | `widgets/<id>.json`, `widget_notes.json` ✅ | yes | data only, checked by the loader and described by `docs/widget.schema.json` |
| AI provider profiles | `ai.json` ✅, private atomic write | no | no keys, only a marker that a key exists; the keychain accounts `ai:<id>` are not synced either |
| AI conversations | `chats/<id>.json` ✅, one file per chat, owner-only, atomic, written at message boundaries | no | terminal context is stored as a marker unless `ai_persist_context` says text; deleting a chat is an unlink, not a secure overwrite; a damaged file is moved aside as `.bad` and kept until the chat is deleted |
| Secrets | OS keychain | only inside the encrypted payload | provider keys are the exception: they stay on the computer they were typed on |
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
set of widget kinds. There is no arbitrary script execution. The loader (`core/src/widgets.rs`) validates every file field
by field, and `docs/widget.schema.json` (JSON Schema, draft 2020-12) describes the same documents for an editor; a test,
`core/tests/widgets_schema.rs`, gives the schema and the loader the same documents and fails when they differ.

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

The **chat** is a block of kind `chat`: its path is the id of its conversation and its source is always this computer. The
messages are kept by the conversation store, never in `pane_blocks.json`. The same chat can be shown in the sidebar column
instead (not a block, not in the layout file, one place at a time). A finished reply is Markdown that the core renders
(`markdown_render`) and the page copies node by node through its own allow-list.

The **monitor** is a block of kind `monitor`. Its source is this computer or a saved SSH connection; for a connection it is
sampled through the exec session of that connection with one fixed command that returns names and numbers only. Linux
hosts are covered; another system is shown as not supported.

## 10. Security model

| Asset | Threat | Control |
|---|---|---|
| Passwords, keys, tokens | read from disk | OS keychain only; profile files hold references |
| Terminal output | sent to an AI service unasked | consent is per pane: the terminal's content leaves the device only after the user shares that pane, and every send still shows exactly what goes out; local and on-device providers ask the same way unless the user turns that off in settings; no provider is chosen for the person, and none exists until one is set up |
| Synced settings | read by the server | end-to-end encryption, the server sees ciphertext and sizes |
| The PTY | input injected by page content | strict CSP, no untrusted HTML in the main page, previews sandboxed |
| Layout / settings files | corrupt or hostile file crashes the app | validated on load, fall back to a fresh state |
| Paste | control sequences run commands | bracketed paste; warn on multi-line paste to a shell that does not support it |
| Everything the app is | telemetry, an update channel or an account arrives through a dependency or a plugin (DESK-26) | none exists; `core/tests/project_policy.rs` pins the dependency lists and the closure of the app, scans first-party sources and the page for network APIs and fails on a CI workflow |
| Provider keys | read from a file, a log or the page, or sent to another host than the one they were saved for | the OS keychain only, under the account `ai:<id>` together with the address they were saved for (a changed address needs the key again); write-only, because no command returns a key; every type that holds one hides it in `Debug`; never synced and never in `ai.json` |
| Model output | a reply runs code or loads something in the page | untrusted text; it is rendered by the core's sanitiser and copied through the page's own allow-list; no image or media element ever; a link with a query string or user information is inert |
| Agent actions | a tool call does something the person did not see or approve | typed tools; each call shows its exact fields and a hash; only the window decides (`ai_decide`, and `ai_undo` for a file change), by repeating the card's hash, and the core checks again when the call runs; nothing is remembered; after untrusted text entered a turn every side-effect tool needs a fresh card; a deny list protects the app's own data, configuration and socket folders, shell start files, `~/.ssh`, `.git/hooks` and `.env` files; a command runs in a separate process with a scrubbed environment and is never typed into a pane; `wsh` and the local socket can neither approve nor consent |
| AI traffic | a request leaves that nobody asked for, or goes to another place than the person chose | one HTTP chokepoint (`core/src/ai/http.rs`): https only (a loopback address, and the local network by an opt-in), no redirects, the resolved address pinned for a destination on this computer or the local network, bounded sizes and times, error text without URLs; nothing is requested before Send, and a name lookup happens only on an act that needs the destination; `Local only` refuses every destination that leaves the computer; the page never talks to a provider and the content security policy is unchanged |

### AI chat

**What leaves the computer.** The text you type, the context you ticked after you shared that pane, and the results of the
tool calls you allowed: nothing else, and nothing before you press Send. A key is sent only to the address it was saved for,
in one header.

**Consent.** A send that carries terminal content goes through four steps: the state of the pane (what it can offer), the
share, the preview and the send by ids. The share is per pane and per route: it ends when the provider, the model or the
address changes, and when the pane or the chat closes. `ai_local_context` set to `auto` removes the share step for a
destination on this computer or on the device and for nothing else; `ai_local_only` refuses every destination that leaves
the computer. The page gets labels, sizes and findings, never the text of an item, and sends ids, never text. A file is
offered as saved, never as the unsaved text of an editor. The marks that bound the last command's output are written by
programs, so they can be forged; they can only select text that is already on the screen.

**A tool call.** The core proposes the call and the window shows a card with the exact fields and a hash. The decision
repeats the hash, the core resolves and checks the call again, runs it and returns the result; nothing is remembered for the
next call. A write shows the diff against the file as it is, carries the stamp of that file, and is refused when the file
changed after the card; the undo log is in memory. A command shows the command and the working directory, runs through a fixed
interpreter with an allow-list environment, with no input, in a process group that a stop can kill, with capped output, and
without an operating-system sandbox.

**`wsh ai`.** A request from a shell is ask-only: no tool is declared, so nothing can be read, written or run because of it.
It is confirmed in the window unless its destination is on this computer and `ai_local_context` is `auto`, and the answer
is printed after the characters a terminal could act on are removed.

**What is cleaned.** Everything that goes to a provider passes the gate: control and format characters are removed and
secrets of known shapes are replaced by typed markers. That is best effort and no guarantee: a secret with no known shape,
split by a line break or encoded can pass.

**Who can decide.** Only the window, and only a press that a person made. A compromised page could consent or approve for
itself; that is accepted and bounded by the page controls (the strict content security policy, the allow-lists, one decision
per card, the hash). The page's allow-list for model output is checked against the core's by a test and is the stricter: the
core passes relative links, the page only absolute `http`, `https` and `mailto` links.

**Accepted residuals.** A process of the same account can read the memory and the environment of the app and ask the
keychain; a command of the agent has the rights of the account and no sandbox (a daemon that leaves its process group
outlives the kill); a name lookup of a host-name provider happens between the last check and the chokepoint's own resolve; a
test that proves that nothing happened proves it for a time window, never for ever.

## 11. Platforms

| | macOS | Windows | Linux | iOS | Android |
|---|---|---|---|---|---|
| Webview | WKWebView | WebView2 | WebKitGTK | WKWebView | System WebView |
| Local shell | ✅ | written (ConPTY), not yet run | written, not yet run | ❌ | ✅ |
| Keychain | Keychain | Credential Manager | Secret Service | Keychain | Keystore |
| Apple AI | ✅ macOS 26 or later with Apple Intelligence, linked weakly so that older macOS runs the app without it | – | – | later | – |
| AI chat | ✅ (test servers only) | written, not yet run | written, not yet run | no key store yet, so the chat refuses to send | same |
| `wsh ai` | ✅ Unix socket | named pipe, no read timeouts | ✅ Unix socket | – | – |

Differences are confined to the shell crate and to implementations of the traits in section 4. The layout model and
the frame protocol are identical everywhere; mobile shows one block per screen from the same `View`.

The AI chat core builds for mobile, but provider keys need a mobile secret store, so the chat refuses to send there. The chat
and its cards were drawn and tested in two browser engines (WebKit and Chromium) and are unverified on WebView2 and WebKitGTK
(rows AI6 and AI7 of [test-plan.md](test-plan.md)); the Apple model is a crate that only the macOS build of the shell links,
and the real model has not been run (rows AI-A1 to AI-A4).

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
  `-EncodedCommand`) that announces the directory with OSC 7, writes the command marks of OSC 133 and reports each new
  command to the history through `wsh` (automatically in holzWave's own PowerShell panes), and the reader that accepts the
  directory report only as a drive path of this machine. A pane ends when its shell does even if the console host never closes the output (`PaneSpec.exit_poll`,
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
- **AI chat:** three release gates and the corpora of the earlier steps. The Rust security corpora (egress, injection, the
  canary key, approval bypass and the wire corpus of the socket; 58 tests over a shared rig, driven from the outside of the
  service the way the shell builds it); the page and access-control gate (an XSS corpus of 66 cases in ten classes replayed
  through the core's renderer and the page's copy on WebKit and Chromium, the allow-lists, the content security policy, the
  capability triple of every AI command, and the touch targets); and the provider matrix (five wires and the gateways, 19
  cells, through text, tool and signed-reasoning turns, with exact headers, error classes, hostile local servers and the
  Apple registry). The human rows that no suite can run are in Part 7 of [test-plan.md](test-plan.md).

  | AI chat stream path | Limit asserted | Measured |
  |---|---|---|
  | cost per event through the provider, 100 000 events per wire | at most 10 µs | 0.19 µs (Ollama) to 0.73 µs (Gemini); Apple silicon Mac, release, three runs, 2026-10-17 |
  | time to first delta through the provider, warm, median | at most 2 ms | 0.16 to 0.48 ms; the same Mac and runs |
  | time to first delta through the provider, cold, median | at most 2 ms | 0.16 to 0.39 ms; the same Mac and runs |
  | the bare client over loopback: first delta warm (median), first delta with a fresh client (median), cost per event | at most 10 ms, 50 ms, 50 µs | 0.22 to 0.23 ms, 0.78 to 0.79 ms, 12.0 µs; the same Mac and runs. A Linux x86_64 virtual machine, release, 2026-10-16: 0.32 ms, 0.77 ms, 1.17 µs |
  | cancel: the call returns, the socket closes | within 250 ms | 5 to 22 µs, 90 to 95 µs; the same Mac and runs. The Linux machine: 8 µs, 60 µs |
  | read of a whole screen as text: the full budget (250 columns, 2000 rows, every cell styled), a 200 by 50 screen | at most 100 ms for the full budget | 17.5 to 19.8 ms, 0.24 to 0.26 ms; the same Mac and runs |

  The ceilings are asserted in release builds only. The numbers are from the machines named in the rows, not from the
  owner's machines; the commands that measure them are in Part 1 of [test-plan.md](test-plan.md) and row AI8 records the
  owner's.
- **Robustness:** corrupt `layout.json`, closing panes mid-output, and resizing during a flood are explicit tests,
  as are hostile `settings.json`, `session.json` and theme files (size, type, range and name limits).
- **What no suite here can see** (real window chrome, an input method, a physical layout, the native menu, fonts, the
  process table after quit) is one human checklist, [test-plan.md](test-plan.md), with a per-platform list for
  Windows and Linux (D-22).

## 13. Build and release

- Toolchain: Rust stable, Zig 0.16.0 for `libghostty-vt` (0.17 breaks Ghostty's `build.zig`), Node and `tsc` for
  the frontend. `libghostty-rs` is pinned by git revision. On a Mac the Swift toolchain (Xcode or the Command Line Tools)
  builds the Apple bridge crate with the workspace; the Foundation Models code is compiled only when the installed SDK has
  the framework (Xcode 26 or newer), and an older SDK builds the app without the model ([building.md](building.md)).
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
| A11 | One HTTP chokepoint with hand-written wire adapters and no LLM SDK | one file knows the client, so the policy test can name it, and no vendor library brings its own network stack or telemetry |
| A12 | Consent is a type: a provider accepts only context the gate approved, and the page sends ids, never text | a bug in the page cannot send what the person did not see |
| A13 | Approval and consent are webview commands only; `wsh` can submit a request but never decide it | every process in a pane holds the token of the socket |
| A14 | The Apple model is a crate with a Swift package behind a C interface, linked weakly and registered only where it is available, not a plugin, with no silent fallback | Tauri's Swift plugins are iOS-only, an older macOS must start the app, and a request must never go to another provider unasked |
| A15 | AI events carry a sequence number per turn and a snapshot command, not a channel | the webview can reload at any time |
| A16 | No remembered approvals | every side effect is decided again, on the exact fields |

## 15. Open questions

1. Tauri 3 stability for iOS and Android, to be checked when phase 9 starts.
2. `opendal` against `reqwest_dav` for sync (size and TLS stack).
3. ~~Whether the frame diff needs per-cell damage or per-row is enough.~~ Answered in Phase 2: per row, compared
   against the rows last sent (section 6); a full-frame encode costs 0.47 ms on 160x50.
4. SQLite crate choice (`rusqlite` against `sqlx`) with the dependency budget in mind.
5. Tauri 3 alpha links `reqwest` unconditionally, for a dev-server proxy that a desktop build never creates. The policy
   test names it as an exception with its exact parents: `tauri` and, for the AI chat client of Phase 7,
   `holzwave-core` (`core/src/ai/http.rs`). When a later Tauri 3 release makes the proxy optional, only the Tauri side
   goes; the AI chat client stays.
6. The wire fixtures of the five providers are written from the vendors' documents, and real streams may differ. Rows AI1
   and AI2 of [test-plan.md](test-plan.md) close this; a difference becomes a new fixture and a rule.

See also: [design.md](design.md) for colour, type, components and the window direction.
