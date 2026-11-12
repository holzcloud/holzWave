# Architecture

Status: **plan**, with the parts built marked ✅. The layout of crates and the boundaries between them are fixed; the
contents of the later phases are a plan and will move. Sections 3, 5, 6, 7, 8 and 12 describe the desktop MVP as it was
built in Phase 2 (macOS, Windows and Linux; the author has run all three). The AI chat, the Apple on-device model and the
extras of Phase 7 are built and tested against in-process servers, scripted providers and a page stub; they have not been
run with a real provider account, and the rows of Part 7 of [test-plan.md](test-plan.md) are the
owner's run. The native integrations of Phase 8 (Kubernetes, Talos, Azure CLI and GAM7, section 9) are built and tested
against fake tools and recorded fixtures; they have not been run against a real cluster, a real Talos, Azure or GAM account,
or on Windows and Linux hardware, and the rows of Part 9 are the owner's run.

## 1. Goals and non-goals

**Goals**

- One terminal core for macOS, Windows, Linux, Android and the browser (served by `holzwave-server`). Platform shells stay thin.
- Fast and small: one draw call per pane and frame, a few MiB of RAM per pane, input latency below one frame.
- Replaceable parts: the VT engine, the connection type, the renderer, the AI provider and the sync backend each sit
  behind a narrow interface.
- Secure by default: secrets only in the OS keychain, the terminal's content only leaves the device with consent,
  the webview has no more power than it needs.
- Few dependencies. Security-critical code (SSH, cryptography, VT parsing) is never hand-written.

**Non-goals**

- A web browser block.
- iOS (no Apple developer account; the sandbox would also forbid a local shell).
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
 │  conn        Link a pane talks to: pty · ssh (russh), one login per saved profile       built          │
 │  secrets     SecretStore trait over the OS keychain                                     built          │
 │  store       SQLite: settings, history, profiles                                        phase 2/7      │
 │  sync        encrypted sync over WebDAV or a holzWave server, conflict handling         built          │
 │  preview     file browser, markdown, media, editor backends                             built          │
 │  control     socket server for wsh                                                      built          │
 │  ai          Provider trait: HttpProvider on five wires · Apple bridge · scripted       built          │
 │  integrate   closed verb catalogue: kubectl · talosctl · az · gam                       built (fakes)  │
 └──────────────────────────────────────────────────────────────────────────────────────────────────────┘
                           ▲
                    wsh (Rust binary) ── Unix socket / named pipe ── also over SSH tunnels
```

The rule that makes this work: **the core owns all state; the webview mirrors it.** The webview holds no
authoritative layout, session or setting, and can be reloaded at any time.

## 3. Workspace layout

| Path | Crate / package | Role | State |
|---|---|---|---|
| `core/` | `holzwave-core` | everything above except the shell | ✅ engine, pty, pane, layout, settings, session, connections, files, sync, the AI chat, the native integrations |
| `app/src-tauri/` | `holzwave-app` | Tauri command layer, window, plugins | ✅ |
| `app/src/` | TypeScript | chrome, renderers, input | ✅ |
| `spike/` | `holzwave-spike` | measurement tool, not shipped | ✅ |
| `wsh/` | `wsh` | CLI (`view`, `edit`, `setmeta`, `ping`, `hist`, `init`, `ai`); depends on the small `holzwave-ipc` crate (`ipc/`: protocol, socket server and client, streamed replies) | ✅ |
| `plugins/apple-ai/` | `holzwave-apple-ai` | Foundation Models bridge: a Rust crate with a Swift package behind a C interface, macOS only, linked weakly, not a Tauri plugin; the whole AI feature then runs offline on the Mac | ✅ |
| `host/` | `holzwave-host` | the commands, the state and the host modules of the shell with no Tauri in its tree: the one command table, the registry that answers a command by name, the AI host, the integration host, the windows, the sync runtime. The desktop and the server both use it | built, tested on the Mac and in a Linux container |
| `server/` | `holzwave-server` | the browser build: the supervisor, the HTTP and WebSocket edge, TLS and ACME, sign-in, the store, the audit log and the worker that runs one person's terminals. One binary with the commands `serve`, `keygen`, `check-config`, `healthcheck` and `worker` (section 16) | built, tested in a Linux container; not run on a real cluster |
| `spawn-helper/` | `holzwave-spawn-helper` | the small privileged helper that starts a worker as a person's own user id, with four file capabilities (section 16) | built, tested in a Linux container |
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
| `local_network` | `local_network.json`: the choice made at the notice for the macOS Local Network permission (`unasked`, `declined`, `requested`), machine-local and never synced, and the rule that nothing probes the network before `Allow` | ✅ |
| `ai` | the AI chat core (`core/src/ai/`, below): the `Provider` trait and `HttpProvider` over five wire adapters in `ai/wire/` (`anthropic.rs`, `gemini.rs`, `ollama.rs`, `openai_chat.rs`, `openai_responses.rs`), the one HTTP chokepoint `ai/http.rs`, the context gate `gate.rs` and `redact.rs`, tools and approval in `tools/`, the agent loop `agent.rs`, the conversation store `store.rs`, profiles and keys `profiles.rs`, the service facade `service.rs` with `service/` | ✅ |
| `textread` | reads the screen, the selection, the last rows or the last command's output as text, with the cells nobody could read marked | ✅ |
| `exec` | runs one command in a separate process with a scrubbed environment, a time limit and a kill of everything it started | ✅ |
| `conn` | the `Link` trait a pane reads and writes (a PTY, an SSH shell, a session of a holzWave server, nothing for a block pane) and the connection state the page shows | ✅ |
| `remote` | the client of a holzWave server's sessions (section 16): `proto` (the vocabulary and every bound of the device socket, shared with the server), `ws` (a bearer WebSocket over the core's own TLS stream), `hub` (the one socket of a device and the subscriptions on it) and `link` (a session as the `Link` of a pane) | built |
| `server_link`, `server_signin` | the per-device link to a server (`server_link.json` and the keychain account `server:token`, a token type that prints only a marker) and the client of the device grant | built |
| `ssh` | the SSH shell on a pooled connection (`SshLink`, with reconnect and a shell backlog capped at 16 MiB), the login itself, host keys, and the exec primitive: one command on a saved connection without a terminal | ✅ |
| `pool` | one login per saved SSH connection, shared by the pane's shell and the SFTP and exec channels: single-flight logins, a wait after a failed background login, idle close, at most 16 connections | ✅ |
| `connections` | the saved SSH, WSL and Docker connections, their secrets, and which pane runs which connection | ✅ |
| `blocks` | panes that are not a terminal: a file browser, a preview, the system monitor and a chat | ✅ |
| `monitor` | the system monitor of this computer (`sysinfo`) and, in `monitor/remote.rs`, of a Linux host through the exec primitive | ✅ |
| `widgets` | the sidebar widgets: data files checked field by field, described by `docs/widget.schema.json` | ✅ |
| `history` | the command history in SQLite, a fuzzy matcher, each use stored with the host it ran on | ✅ |
| `integrate` | the native integrations (`core/src/integrate/`, section 9): `args` (validated names), `detect`, `run` (the one place a tool is started), `view` (the view model), `profiles`, `context`, `ticket` (the card and the approval), `service`, and one module per tool: `kube`, `talos`, `azure`, `gam`. Tested against fake tools and recorded fixtures | ✅ |

The AI chat in the shell and the page (`holzwave-core` itself stays free of Tauri and of any PTY code):

| Where | What |
|---|---|
| `app/src-tauri/src/ai_host.rs` | the host: `AppCore` holds one service, answers the `ai_*` commands, supplies the context source and the tool host, and registers the Apple provider only where it is available |
| `ai_tools.rs`, `ai_write.rs`, `ai_exec.rs` | the shell side of the agent's tools: file reads, writes with a diff and an undo log, commands in a separate process or through the exec session of a saved connection |
| `ai_ask.rs`, `ai_device.rs` | the confirmation of `wsh ai` requests, and the one question the shell asks of the Apple model (`DeviceSource`) |
| `wsh/src/ai.rs`, `ipc/src/stream.rs` | `wsh ai` and the streamed reply of the local socket |
| `plugins/apple-ai/` | `src/{backend,native,prompt,provider,pump,scripted}.rs` over `swift/Sources/AppleAI/` |
| `app/src/` | the chat (`chat`, `chatview`, `chatstate`, `chatlist`), the sidebar form (`aisidebar`), the providers dialog (`aisettings`, `aistate`, `ondevice`), the macOS local network notice and Settings group (`localnet`, `localnetstate`, `localnet.css`), consent (`consent*`), approval and undo (`approval*`, `undo`), `wsh ai` requests (`ask*`), the palette actions (`inline*`), and the monitor and history views (`monitorstate`, `historystate`, `historyview`) |

The integrations in the shell and the page:

| Where | What |
|---|---|
| `app/src-tauri/src/integrate_host.rs` | builds the integration service from the real machine (environment, file system, login shell, keychain), answers the `integration_*` commands, turns a refusal into the fault object and sends the event `integration`; it holds no policy of its own and starts no process |
| `app/src/` | the block (`integrate`, `integrateview`, `integratestate`, `integratetools`), the Context section and the status segments (`integratecontext`, `integratepick`), the confirmation card (`integrateconfirm`), the profiles dialog (`integrateprofiles`) and `integrate.css` |

The app shell keeps one more type of its own, `AppCore` (`app/src-tauri/src/state.rs`): it holds the layout, the
running panes, the settings and the session, has no Tauri type in it, and is tested headless with a recording sink
and real shells. `lib.rs` and `commands.rs` only adapt it to Tauri.

## 4. Core boundaries

Each boundary is a trait in `holzwave-core` with at least one real implementation and a test double.

| Trait | Methods (sketch) | Implementations | Why a boundary |
|---|---|---|---|
| `TerminalEngine` ✅ | `feed`, `resize`, `scroll`, `update` (dirty rows), `take_replies`, `encode_key`, `encode_text`, `paste`, `mouse`, `wheel`, `select`, `search`, `copy_mode`, `set_theme` | `GhosttyEngine` ✅, `AlacrittyEngine` | `libghostty-vt` is young and its API moved already between 0.2.2 and master |
| `Connection` (`core/src/pool.rs`) | `session(open)`, the shell channel (`SshLink::attach`), `sftp()`, exec channels through `SshExec::on`, `lease()` | SSH, through the pool: one login per saved profile. WSL and Docker are local commands that run in a pane (`wsl.exe -d`, `docker exec -it`) and have no file access yet (open) | the terminal, the files and the commands of a saved SSH connection share one login |
| `SecretStore` | `get`, `set`, `delete` | macOS Keychain, Windows Credential Manager, Secret Service; on Android a vault: one file of ciphertext only, its data key wrapped by a key of the Android Keystore, with the states new, locked, unlocked and lost ([build-android.md](build-android.md#the-secret-store)) | secrets never reach a file in plaintext |
| `Backend` (`core/src/sync/webdav.rs`) | `ensure`, `list`, `get`, `put` with a condition on the ETag (absent, match, any) | `WebDav` over `ureq` (Nextcloud and other WebDAV servers) and `ServerBackend` (a holzWave server, `core/src/sync/server.rs`) | the engine and the format do not know which target they talk to |
| `Provider` ✅ | `id`, `capabilities(model)`, `stream(request, callback, cancel) → completion` (blocking, with a cancel token) | `HttpProvider` ✅ over five wire adapters (local servers, cloud APIs, gateways), `AppleProvider` ✅ (Foundation Models), `ScriptedProvider` (the test double) | consent and context rules live above it: a provider accepts only context the gate approved |
| `Integration` ✅ | `spec`, `argv`, `view` (section 9) | `Kube`, `Talos`, `Azure`, `Gam`, each built and tested against fake tools and recorded fixtures | one engine and a closed verb catalogue for four tools |

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
- An integration tool runs as a child process of the one runner (`integrate/run.rs`) with a reader thread for each of its
  two pipes; at most four children run at once and a fifth waits at most a second. A background thread named
  `holzwave-integration-status` reads the default verb of one due tool at a time, started by the first ask of the page, and
  has no timer: an idle or hidden app starts nothing. A terminal launch is an ordinary pane actor whose program is the tool;
  a failed start is held on screen until Enter.
- The exec primitive (`SshExec`) runs on one worker runtime of its own, a single worker thread named `hw-exec`, with a cached
  connection and no terminal; its callers only wait.
- The sessions of a holzWave server in an app go through one thread, `hw-hub`, that owns the device's one socket to its server:
  it connects (blocking by design, because the platform's check of a certificate can take long, and never on a caller's
  thread), reads with a short timeout while a subscription exists (2 ms, 8 ms after a second of silence) and a long one
  otherwise, sends what the panes hand it over a channel, matches answers to requests and routes each binary frame to the
  subscription of its session. A subscription's queue is counted in bytes (24 MiB by default); one that cannot keep up is
  attached again from a fresh snapshot, never buffered without bound. Measured on loopback in a debug build: the app adds about
  3 ms to the median time from a typed byte to the frame that shows it, and a silent attached session costs 0.1 to 0.4 percent
  of a core (`server/tests/remote_budget.rs`); a release build was not measured.
- In a worker of the server, the session host has one housekeeping thread, `hw-sessions`, which frees a writer's right at its
  deadline, notices a program that ended and removes ended sessions, and sleeps until the next deadline (between 5 ms and
  60 s). Each attachment's stream is read by a thread of its own (`hw-stream-<session>`): the snapshot in chunks, then the
  bytes of the pane's tap, which the engine thread fills without ever waiting for a reader. The browser window's sessions
  have one more thread, `hw-local-sessions`, which beats for the window's attached panes while a browser tab is open and
  attaches a pane that fell behind again from a fresh snapshot. The local command runner (`exec.rs`) uses plain threads for its two
  pipes and is polled, not blocked, for the end, the time limit and the stop.

## 6. The app protocol

The webview talks to the core through a deny-by-default set of 147 commands and 17 events. The tables below list the
commands of the terminal core, of the AI chat and of the integrations; the golden lists every command. Seven commands of the
desktop app (`server_status`, `server_refresh`, `server_sign_in_start`, `server_sign_in_poll`, `server_sign_in_cancel`,
`server_sign_out` and `sync_use_server`) link the app to a holzWave server, and six more (`server_sessions`,
`server_session_new`, `server_session_open`, `server_session_end`, `pane_takeover` and `pane_load_older`) list, make, open and
end the sessions of that server and run them as panes. The browser build has none of the seven: it is the server's own page
and its sign-in is the credential. It has the six of the sessions, which the person's worker answers from its own sessions,
and the server answers `server_sessions`, `server_session_new`, `server_session_open` and `server_session_end` with
`Remote sessions are off on this server.` before the worker sees them while that switch is off. A pane of a server session reports
who writes, the session's grid and what it is busy with in the `remote` member of its `pane_connections` line. The Rust types are the
source of truth; `app/tests/fixtures/protocol.json` is the golden written from them (`UPDATE_FIXTURES=1`) and read by
the TypeScript tests too, so a renamed argument or a changed payload fails on both sides at once. Arguments are
snake_case everywhere (`cell_w`, `allow_unsafe`).

**Commands** (webview → core), addressed by `pane` where they concern one:

| Group | Commands | Answer |
|---|---|---|
| Layout | `get_layout`, `split(column)`, `new_tab`, `new_workspace(name)`, `close_pane(pane)`, `focus_pane(pane)`, `cycle_focus(step)`, `select_tab(index)`, `select_workspace(index)`, `set_ratio(split, ratio)` | the new `View` |
| Pane lifecycle | `start_pane(pane, cols, rows, cell_w, cell_h)`, `resize(pane, cols, rows, cell_w, cell_h)` | nothing (`start_pane` waits for the shell to start) |
| Input | `key(pane, code, key, mods, repeat, release, composing)`, `text(pane, text)`, `paste(pane, text, allow_unsafe)`, `paste_cue(pane)`, `clipboard_text`, `mouse(pane, action, button, x, y, mods)`, `wheel(pane, rows, x, y, mods)` | nothing; `paste` and `paste_cue` answer `written`, `empty` or `rejected`; `clipboard_text` the system clipboard's text (empty when it holds none, an error where the shell has no native clipboard) |
| Viewport and frames | `scroll(pane, rows)`, `scroll_to_bottom(pane)`, `request_full(pane)`, `image_data(pane, image, generation, offset)`, `set_image_animation(enabled)` | nothing; `image_data` the pixels of a picture as raw bytes (a 16-byte header, then at most 512 KiB); `set_image_animation` takes no pane |
| Selection, search, copy mode | `select(pane, op)`, `selection_text(pane)`, `copy_selection(pane)`, `search(pane, op)`, `copy_mode(pane, op)` | the text, whether the core's selection was written to the system clipboard (no argument carries text), `{total, current}`, `{active, selecting, yanked}` |
| Settings | `get_settings`, `set_setting(key, value)`, `reset_setting(key)`, `reload_settings` | the settings view |
| System | `app_info`, `open_url(url)`, `reveal_path(path, base)` | the version, OS and host name; nothing |
| AI conversations | `ai_conversation_new`, `ai_conversation_list`, `ai_conversation_delete(conversation)`, `ai_send(conversation, text, pane, profile, model, offer, picked)`, `ai_cancel(conversation)`, `ai_turn_snapshot(conversation)` | an id (no file exists until the first send); the stored chats with a note for each file that cannot be used; whether the chat was removed; a ticket for the turn (`ai_send` answers at once and the reply arrives as `ai` events); whether a turn was stopped; the whole state of one chat |
| AI providers and keys | `ai_profiles`, `ai_profile_save(input)`, `ai_profile_remove(profile)`, `ai_key_set(profile, slot, api_key)`, `ai_key_clear(profile, slot)`, `ai_has_key(profile, slot)` | the profile views with the state of the on-device model; the id of the saved profile; whether the profile and its key are gone; nothing; nothing; whether a key is set. A key goes in and never comes out |
| AI context | `ai_context_state(conversation, pane, profile, model)`, `ai_context_share(conversation, pane, profile, model, shared)`, `ai_context_preview(conversation, pane, profile, model, kinds)` | what the pane can offer and whether it is shared (twice); an offer: labels, sizes and findings, never the text |
| AI decisions | `ai_decide(conversation, call_id, decision, hash)`, `ai_undo(conversation, call_id)` | nothing; webview commands only |
| `wsh ai` requests | `ai_ask_pending`, `ai_ask_answer(ask, verdict, seal)` | the requests that wait, nothing |
| Markdown | `markdown_render(text)` | the cleaned HTML of a reply |
| Integrations: tools and reads | `integration_tools`, `integration_run(tool, verb, pane, args)` | the four tools with where each stands on this computer (path, version, the places looked in, the verbs each has); one read of a verb that shows a view: the view model, the selection it used and when it was read. A refusal is the fault object |
| Integrations: selections | `integration_select(tool, pane, selection)`, `integration_context(pane, refresh)` | the selection as it will be used, with the name of the production profile or `null`; `{rows, panes}`: one row for each tool, and one entry for each pane that has an integration selection, with the status of its tool when the pane holds the tool's last words |
| Integrations: verbs that change something | `integration_prepare(tool, verb, pane, args)`, `integration_confirm(ticket, hash)`, `integration_cancel(ticket)`, `integration_launch(tool, verb, pane, args, placement)` | the card of a verb that changes something or opens a session (nothing is started); `{kind, pane, note, view}`: `ran` with the tool's first line, or `launched` with the new pane and the layout it is in; nothing; `{pane, view}` of a terminal for a verb that only reads |
| Integrations: programs | `integration_binary_set(tool, path)`, `integration_binary_clear(tool)` | the four tools again; a program is accepted only when its file name is one of the tool's own, it is an absolute path and the core finds it usable |
| Integrations: profiles and secrets | `integration_profiles`, `integration_profile_save(profile)`, `integration_profile_remove(id)`, `integration_secret_set(profile, name, value)`, `integration_secret_clear(profile, name)` | the profiles and the notes about the files that could not be used, where a secret variable is a name with `has_value` and never the value (saving and removing a profile answer them again); nothing for the two secret commands: a value goes in and never comes out |
| `wsh` on a host | `wsh_install(pane)` | the sentence the pane also shows; the page names the pane, never a host |
| Secret vault (Android) | `vault_status`, `vault_unlock`, `vault_lock`, `vault_protect(on)`, `vault_reset`, `vault_hidden(hidden)` | `{state, protection, has_screen_lock, strongbox, can_protect, notice, reason}` in words (`state` is `none` on a host that has no vault); a failure is one of the words `cancelled`, `lost`, `no_screen_lock`, `failed`, or the sentence `no secret vault on this device`; never a key, a secret or the text of the keystore. The browser build does not have them |
| File hand-over (Android) | `handover_in`, `handover_out(path)` | `{kind: done, name, size, shown, renamed}`, `{kind: cancelled}`, `{kind: too_large, name, size}`, `{kind: failed}` or `{kind: unavailable}` (a host with no document dialog); never the text of a failure or of a provider, and `name` is the cleaned one. One file at a time, at most 256 MiB, into `handover/` of the shell's folder (never overwriting) or out of it (a regular file inside the folder only). The browser build does not have them |

Every refusal of the AI commands is an object with `code`, `reason` and `text` (the page words its own sentence from the
code and the reason and shows the text only as the core's plain sentence); a plain string is the shape of the older commands. The
integration commands refuse the same way, with the codes `refused`, `missing`, `failed`, `timeout`, `overflow`, `busy`, `ticket`,
`profile`, `unsupported` and `parse`.

The `op` payloads are internally tagged with `kind` and snake_case names (`{kind: "begin", x, y, mode: "word"}`,
`{kind: "move", motion: "line_start"}`). The old `input` command (raw bytes to any PTY) is gone, and there is no
`set_theme`: the theme is the `theme` setting, changed with `set_setting`. Commands that wait for the engine thread's
answer (up to 2 s) or start a process are `async` and never run on the main thread; `mouse` and `wheel` are hot paths
that only bound their numbers and push to the pane's queue.

**Events** (core → webview), emitted to every webview as JSON unless the row says otherwise:

| Event | Payload |
|---|---|
| `frame` | a frame message of one pane (below) |
| `layout` | the `View`: workspaces, tabs of the active workspace, panes and dividers of the active tab in unit rectangles; every workspace and every tab also lists the ids of its panes (`pane_ids`), so the page can map a pane of another tab to its tab and its pill |
| `title` | `{pane, title}` from OSC 0 and 2, cut to 4096 bytes without control characters |
| `cwd` | `{pane, cwd}` from OSC 7, absolute paths only |
| `pane-exit` | `{pane, code}`, `code` is `null` when the exit status is unknown |
| `bell` | `{pane}`: the program in that pane rang the bell (BEL), at most once per 500 ms per pane; the window that has the pane marks the tab and the pill (design 6.23) |
| `command-end` | `{pane, code}`: a command ended in that pane (the `D` mark of OSC 133), at most once per 500 ms per pane; `code` is its exit status, `null` when the mark carried none; the window that has the pane marks the tab and the pill (design 6.23) |
| `settings` | the settings view, after a command or a hand edit that was picked up |
| `incoming-drop` | `{kind, pane, x, y}`: a pane or tab released over this window comes from another window; the page answers with `attach_pane` or `attach_tab`, or with nothing |
| `drag-hover` | `{kind, pane, x, y}`: a pane or tab being dragged out of another window is over this one; the page draws the outline of the zone it would land in |
| `drag-end` | `null`: that drag left this window or ended; the page takes the outline away |
| `ai` | one event of a chat turn, to every window (the page routes it by `conversation`): `conversation`, `turn`, `seq` (from 1 per turn), `kind` (`turn_started`, `delta`, `reasoning`, `usage`, `trimmed`, `message`, `tool_proposed`, `tool_result`, `turn_ended`) and the fields of the kind |
| `ai-ask` | `{kind: waiting, view}` to the window of the pane that asked, `{kind: closed}` to every window: a `wsh ai` request waits for a person, or is over |
| `history-open` | `{pane}`: the shell of that pane asked for the history search (the `^R` binding); the window that has the pane opens the dialog |
| `login-prompt` | `{id, connection, host, name, instructions, prompts: [{text, echo}]}`: the server of a saved connection asks a question to log in; every window. `connection` and `host` are the person's own saved connection, the rest is the server's text, cleaned and bounded, shown as text only. The page answers with `login_prompt_answer(id, answers)` or `login_prompt_cancel(id)`; an unknown or old id changes nothing |
| `login-prompt-end` | `{id, outcome}`: the question is over (`answered`, `cancelled` or `timed_out`); every window takes its dialog away |
| `integration` | `{seq, kind, tool, pane, code}`, to every window: something the integrations show changed. `kind` is `tools`, `context`, `profiles` or `held`; `tool` and `pane` name what it is about, `null` when it is about none; `code` is the status of the tool in a `held` event. It carries no data: the page asks (`integration_tools`, `integration_context`) and is right again even after it missed an event |

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
  changed,                                        // the rows that differ from the previous frame
  images? }                                       // the pictures on the screen, only when the list changed (below)
```

A row is `{y, runs, extras?, links?}`. A run is `[text, cols, fg, bg, flags]` with flags bold 1, italic 2, underline 4,
inverse 8, link 16. **Every cell contributes exactly one code point to its run's text**: the first code point of its grapheme
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
- **Pictures.** `images` is a list of placements and is in a frame only when the list differs from the one last sent for
  that pane, and on a full frame that has any. A placement is one array of sixteen numbers, in this order: image id,
  generation, layer (0 below the cell backgrounds, 1 below the text, 2 above the text), z, column, row, grid columns, grid
  rows, x offset, y offset, pixel width, pixel height, then the source rectangle (x, y, width, height). Column and row are
  signed: they are negative when a picture starts above or to the left of the viewport. An empty list in a frame that is not
  full means every picture is gone; a full frame replaces everything, so a full frame without the key means none. A frame
  without the key leaves the page's list as it is, and serialises to the bytes a frame had before pictures existed (a test
  compares it with a stored string). At most 256 placements are sent. No frame carries pixels: the page asks for them with
  `image_data(pane, image, generation, offset)`, which answers raw bytes (a 16-byte header with width, height, the total and
  the offset, then at most 512 KiB), and a request for an image or generation the pane does not hold, or a pane of another
  person, is answered with one fixed error text and no pixels. A new generation of the same image id is a change of the list.
- **Links.** A row that holds OSC 8 hyperlinks has a `links` key, a list of `[x0, x1, uri]` with the columns inclusive, and
  the cells they cover carry the run flag 16 (`LINK`) beside the other flags. The core keeps at most 8 spans and 4096 address
  bytes per row, an address of at most 2048 bytes, and 128 KiB of addresses per frame; rows past the frame budget are sent
  without their links. Every address is cleaned before it is sent: printable ASCII only, a scheme, and no `javascript`,
  `data`, `vbscript`, `blob` or `about` address; one that fails is plain text. A row without links has no key and encodes
  as before. A link that wraps from the end of a row to the next is two spans with the same address, one for each row: the
  page does not treat a wrapped link as one object. Whether an address may be opened is not decided here (section 8).

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
- **An integration verb that changes something has two steps, and the window decides the second.** `integration_prepare`
  makes a card and starts nothing; only `integration_confirm(ticket, hash)` runs the verb, once, and only while the
  request still resolves to what the card showed. A terminal for a verb that only reads (`integration_launch`) needs no
  card, and an exec verb is refused there. The page sends facts (a tool, a verb, a pane, names, a selection) and never a
  program, an argument list or a shell string. The reviewed list `INTEGRATION_COMMANDS` in `acl.rs` names every integration
  command and its answer type; its rules fail when a second command names a hash or a ticket, when a command takes a program,
  an argument list or a shell string, when a command other than `integration_secret_set` takes a value, when an answer
  type could hold a secret, and when the `ipc` or `wsh` sources name an integration command, a ticket or an approval.
  Secrets are write-only: `integration_profiles` lists a secret variable by name with `has_value`, and the values reach only
  the environment of the tool that is started.
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

**The web wire.** The browser build carries the same commands and events over one WebSocket, `GET /ws` of the page's own host.
A frame is a JSON text message: a command is `{"id", "cmd", "args"}` with no other member, an answer carries the same `id`, and
an event is `{"event", "payload"}`, the payload being the text the desktop emits. The server looks the name up in the command
table of section 3 and checks the size of the frame against the cap of that command before it asks the person's worker; it never
parses a result. The server adds one event of its own, `hw-session`, with the states `ok`, `expiring`, `locked`, `unlocked` and
`ended` and the seconds that remain, and it closes a socket with a code the page understands: `4401` (no live session, or the
session ended or the account was disabled), `4403` (the Origin or Host is not the public one), `1001` (the server is
stopping, or 50 seconds of silence), `1002` (a protocol error), `1008` (too many violations), `1009` (a message over the cap),
`1012` (the worker ended) and `1013` (too many sockets, or a peer that does not read). The page treats `4401` and `4403` as
final and every other close as a reason to reconnect. The socket sits behind a `Transport` trait in one file, so the framing
library can change without touching the route; compression is not adopted (section 16). The proof is in
`server/tests/ws.rs`, `server/tests/wire.rs` and `server/tests/parity.rs`; the last sends the whole command table over a real
socket and compares it with the desktop's golden.

## 7. Data and persistence

| Data | Store | Synced | Notes |
|---|---|---|---|
| Layout (workspaces, tabs, tree) | `layout.json` ✅, atomic write | yes | repaired or reset when inconsistent (DESK-15) ✅ |
| Settings, key binding overrides | `settings.json` ✅ → SQLite later | yes | read key by key, one bad value costs one key; formats in [config.md](config.md) |
| Themes | `themes/*.json` ✅ | yes | one file per theme, validated; `#rrggbb` colours only |
| Working directories, window frame | `session.json` ✅ | no | machine-local hints; a damaged file costs the hint, never the start |
| Local network notice | `local_network.json` ✅ | no | the choice of the macOS notice (macOS 15 or later only); machine-local, so another Mac asks for itself; a damaged file costs the choice, never the start |
| Connection profiles | SQLite | yes | secret fields hold a keychain key, never the secret |
| Command history | `history.db` ✅, SQLite, owner-only, deleted rows overwritten | no | fuzzy search (`nucleo-matcher`); each use carries the label of the host it ran on (`local`, `ssh:{id}`, `wsl:{id}`, `docker:{id}`). Only commands typed in a pane of this computer or of a saved connection reach it, nothing is recorded from a far shell. It is not synced: deletions of themes and widgets travel (see [sync.md](sync.md)), but the history is a database that cannot be carried entry by entry, so a forgotten command would come back |
| Widgets | `widgets/<id>.json`, `widget_notes.json` ✅ | yes | data only, checked by the loader and described by `docs/widget.schema.json` |
| AI provider profiles | `ai.json` ✅, private atomic write | no | no keys, only a marker that a key exists; the keychain accounts `ai:<id>` are not synced either |
| AI conversations | `chats/<id>.json` ✅, one file per chat, owner-only, atomic, written at message boundaries | no | terminal context is stored as a marker unless `ai_persist_context` says text; deleting a chat is an unlink, not a secure overwrite; a damaged file is moved aside as `.bad` and kept until the chat is deleted |
| Integration profiles | `integrations.json` ✅, atomic | yes | names and flags only; a file that breaks any rule is kept whole as `integrations.json.bad` and the app starts without profiles; described by `docs/integrations.schema.json` |
| Integration paths | `integration_local.json` ✅ | no | the program and the configuration files of each tool on this computer; never a path in a synced file |
| Integration selections and launches | `pane_integrations.json` ✅, `pane_launches.json` ✅ | no | which context, namespace, subscription, section or node a pane uses, and the label of a pane that was a tool session (it is not started again after a restart) |
| Integration private files | `run/kube/<pane>.json` ✅ (mode 0600 in a folder of mode 0700), `integrations/<azure or gam>/<profile>/` ✅ | no | the overlay kubeconfig of a shell pane (names only, removed with the pane) and the configuration folder of an isolated Azure or GAM7 profile (the tool's own tokens, signed in again inside it) |
| Secrets | OS keychain | only inside the encrypted payload | provider keys are the exception: they stay on the computer they were typed on; the secret variables of an integration profile are `int:<profile>:<NAME>`, at most 1 KiB, and travel inside the encrypted payload |
| Scrollback | memory only | no | the engine owns it |

The sync payload and its encryption are specified before any sync code exists: see [sync.md](sync.md).

## 8. Rendering and input

**Rendering.** A pane is a grid; the core never draws. The webview paints all panes of a tab on one canvas:
`begin(bg)`, `drawPane(frame, rect)` per pane, with a scissor box per pane. WebGL2 uses a glyph atlas (R: coverage in
alpha, one slot per glyph and style, two adjacent slots for wide glyphs) and one instanced draw per pane. Canvas 2D
is the fallback when WebGL2 is missing. Both are covered by a pixel test that checks each pane stays inside its
rectangle.

**Pictures.** The terminal engine reads the Kitty graphics protocol at all three levels: still pictures (transmit, display,
place, delete), animations (the engine never advances a frame by itself; the pane gives it a clock and it says when the next
frame is due), and pictures that a program draws with placeholder characters (U+10EEEE with combining marks, which the core
reads as blanks in text reads, copies and searches). Only a transmission that carries the bytes itself is accepted: a
request that names a file, a temporary file or shared memory is refused with an error answer, nothing is read and nothing
is stored. A PNG is decoded by one decoder that is put in front of the library's own and checks the header first, so a
header that claims more than 128 MiB of decoded pixels, or more than 10000 pixels in either direction, is refused before
anything is allocated. Every request is bounded: the memory that one pane keeps for pictures is the setting `image_memory_mib`
(default 64, 0 to 512, counted for each of the two screens; `0` turns pictures off), a server caps it with
`workers.max_image_mib` (default 64, 0 to 4096) and the smaller number holds, a single escape sequence over 8 MiB is refused,
and the terminal's allocator refuses a request that would take a pane past twice the setting plus 64 MiB, which is what bounds
a transmission that is never finished. An escape sequence the engine cannot use is answered as the protocol says and costs
nothing else. The page draws the placements of `Frame.images` in both renderers in three layers (below the cell backgrounds,
below the text, above the text), pulls the pixels of a placement in checked pieces with `image_data`, and keeps the decoded bitmaps of one
pane inside the same `image_memory_mib` and those of all panes together inside four times that. The size of a picture is
checked on its header, before anything is allocated; a picture that does not fit next to the ones in use is not pulled further,
and room is made first from bitmaps that no placement asks for, in any pane, the least recently drawn first, never from a
placed one. A bitmap that no placement asks for any more is let go of after five seconds (one timer for the page, set only while
such a bitmap exists), so a picture scrolled out of view and back is not pulled again, and a picture a program deleted does not
stay until the pane closes. A WebGL texture goes with its bitmap at the next draw, and also when it has not been drawn for 120
frames. A picture whose pull failed for a reason that can go away is asked for again: one that did not fit when memory is freed
or the pane's pictures change, one whose pull failed when memory is freed or the connection opens again (the page does not ask
again by itself, so a failing pull never loops); a picture the core refused for a property of its own (a header that lies, sizes
that do not add up, a piece of the wrong length, a picture over the whole budget) is not asked for again until the budget
changes. The budget counts decoded bytes only, not the second copy that a WebGL texture holds on the GPU. The server's key
`workers.max_image_mib` accepts up to 4096, but a pane never keeps more than 512 MiB (the top of the range of
`image_memory_mib`), so a value above 512 has no further effect. The page tells the core whether the person asked for reduced motion (`set_image_animation`); with it, an animation
stays on one frame. The switch is one per person, not one per pane. These parts are described in
[config.md](config.md) and [server.md](server.md#memory-for-pictures); what ran against what is in section 12.

**Links.** The engine reads OSC 8 hyperlinks and the core sends them as spans in the row (section 6). The page underlines only a link
it treats as one, dotted at rest (the scheme is on the person's list, and a `file` link counts only on the desktop in a pane that runs
on this computer; the core flags every span it keeps and the page clears the flag for the others before either renderer
draws), shows its real address in a `LINK` segment of the status line while the pointer is on it, and opens it on
a modified click (Cmd on macOS, Ctrl elsewhere, either in the browser) through `open_url(url, pane)`; when the text on the
screen names another place than the address, it asks first. `open_url` checks the address again in the core: only the
schemes of the person's list `link_schemes` (default `http`, `https`, `mailto`, `file`) are opened, `javascript`, `data`,
`vbscript`, `blob` and `about` can never be on the list, and a `file` link only shows the file in the file manager and never
opens or runs it, and only for a pane that runs on this computer with an empty host or `localhost`, an absolute path that
exists, and no `..`, control character or encoded slash. A program that writes `file://<machine name>/path` (GNU `ls
--hyperlink` does) is refused for that reason. The browser build opens `http`, `https` and `mailto` links only, in a tab
without an opener.

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
  `selection_text`. When a mouse selection ends the page calls `copy_selection(pane)` (setting `copy_on_select`): the
  shell writes the core's own selection text to the system clipboard (NSPasteboard in process on macOS, bounded at
  16 MiB; elsewhere it answers an error and the page uses the web view's clipboard). The page never passes the text, so
  the command is no clipboard write primitive.

**Replies.** Some sequences need an answer (device attributes, size reports, colour queries). The engine collects
them and the engine thread hands them to the PTY writer thread; a program that waits for an answer does not hang
(DESK-10). Colour queries are answered with the theme's colours and end with the terminator of the query. A reply
over 64 KiB is refused as a whole, never cut, and replies are dropped when the writer is 256 KiB behind.

**Paste.** `paste(pane, text, allow_unsafe)`: the engine always sanitises, brackets the text when the program turned
on bracketed paste (DESK-13), and answers `rejected` for text that could inject commands (a newline without bracketed
paste, or the end marker with it) without writing anything. The engine never decides to allow it. The page then asks
the person (`confirm_multiline_paste`, D-15) and, on yes, sends the same text again with `allow_unsafe: true`.
Every route ends there: the web view's `paste` event, and, when that event does not come within 100 ms of a paste
chord, the text of `clipboard_text`, which the shell reads from the system clipboard (NSPasteboard in process on macOS,
bounded at 16 MiB, an error elsewhere so that the page falls back to the web view's clipboard). The command is called
only from that fallback of the person's own chord and only reads.

An image on the clipboard is no text, so it has no `paste`. The page sends `paste_cue(pane)` instead (a paste event that
holds an image and no text, or, on macOS, a fallback read that found no text): when the program turned on bracketed
paste the engine writes an empty bracketed paste, which Claude Code on macOS reads as the cue to fetch the image from
the clipboard itself; otherwise nothing is written (`empty`). The bytes are fixed and no argument carries content.
`paste` of an empty text still writes nothing in either mode.

## 9. Integrations and extensions

### Integrations (built)

Status: built, and tested against fake tools and recorded fixtures. The four tools, the Context section, the status
segments, the confirmation card with the second press on production, the profiles dialog and the palette entries are
built. They have not been run against a real cluster, a real Talos, an Azure or a Google Workspace account, or on Windows
and Linux hardware: the rows of Part 9 of [test-plan.md](test-plan.md) are the owner's run, and nothing here is called
verified until they pass. The shapes of Talos output and the write of `talosctl config context` are assumptions until rows
INT-T1 and INT-T2. The policy test (`core/tests/project_policy.rs`) holds the code to the rules that can be checked by
reading its sources, and holds this section, the rows and the configuration guide to the code.

**What an integration is.** Kubernetes, Talos, Azure and GAM7 are compiled in. Each is one static `IntegrationSpec`:
its binary names per system, a version probe, the environment it sets, what it lets you select (a context, a
namespace, a subscription, a section, a node) and a **closed catalogue of verbs**. A verb has an effect (`Read`,
`Local` for the tool's own configuration file, `Write` for a cluster or an account, `Exec` for a session with your
credentials), an output (a table, key-value or text view, a terminal block, or nothing) and typed arguments. One
generic engine does detection, running, context, approval and the block for every tool and never knows a tool by name.
Four small modules, one per tool, hold only the argument builders and the parsers: `Integration` has `spec`, `argv`
and `view`. An unknown verb is refused. This is not a plugin host (non-goal 3). A loadable argument template that
travelled through sync would be remote command execution on every device that holds the passphrase, so the two
published JSON documents, the profile file and the block view model, are data with a schema and a parity test (like
`widget.schema.json`), never code: `docs/integrations.schema.json` and `docs/integration-view.schema.json`, each held
equal to its loader by a test.

**The clients.** `kubectl` and `talosctl`, and `az` and GAM7 as wrappers. In-process clients were measured and left
out: `kube-rs` adds 34 crates, 5.9 MB and 35 s to a cold release build and would need a hand-built attach,
exec-credential plugins and the GUI `PATH` handled again; a Talos gRPC client adds 12 crates and a protobuf toolchain,
and there is no official Rust client. The tools themselves handle plugins, proxies and every auth method. The one
dependency the integrations added is the `csv` crate, which reads the output of GAM7 (a person's directory text, so not a
hand-written splitter).

**Where they run.** On this computer only. A remote host is not planned. A tool is found by, in order, the path you
set, the process `PATH`, the `PATH` of your login shell (asked once, lazily, only when the first two found nothing,
absolute existing directories only, never on Windows, and only for zsh, bash, fish, sh, dash and ksh) and a short fixed
list per system; the `PATH` of a child is rebuilt from the same places, so a credential plugin is found too. A program
that the person sets is accepted only when its file name is one of the tool's own.

**The runner.** Every child is started with an argument list: no shell, standard input closed, no program named in the
source. Its environment is the inherited one minus the loader and interpreter variables (`LD_*`, `DYLD_*`, `PYTHON*`,
`NODE_*`, `GIT_*`, `BASH_ENV`, `ENV`, `IFS`) and minus the app's own `HOLZWAVE_*`, plus the profile's plain variables
and its secret ones, and the tool's fixed ones (the Azure run sets `AZURE_CORE_COLLECT_TELEMETRY=false`; the person's own
use of the tool in a terminal is not touched). Credential variables are kept on purpose: the tools need them, and the AI
command runner's rule of dropping every name that looks like a key would break them. Output is capped (16 MiB by default)
and a run that exceeds the cap fails and keeps the head, so a table never shows a cut-off document. Each run has a
timeout (15 s by default) and a process-group kill, and at most four children run at once (a fifth waits at most a
second, then answers `busy`). The one string a shell ever sees is a constant that picks bash or sh inside a pod.

**Connection data.** The tools' own credential stores (kubeconfig, talosconfig, `~/.azure`, `~/.gam`) are referenced
and never imported: the keychain cannot hold them and a materialised file would break "secrets only in the keychain".
`integrations.json` is synced and holds names and flags (profiles, a pinned context or subscription or section, a
production flag, plain environment). Machine-local paths live in `integration_local.json` and never sync. `secret_env`
values are small (at most 1 KiB), live in the keychain as `int:<profile>:<NAME>`, are read at the moment of a spawn and
wiped after it, and travel only inside the encrypted `secrets.json`. A synced profile may not set a variable of the deny
list, `PATH`, a variable the app sets itself or one that makes a tool start another program; a file that fails
validation is set aside whole as `integrations.json.bad` and the app starts without profiles. The format is in
[config.md](config.md).

**Context.** A block passes its selection on every command (`--context=<name>`, `--namespace=<name>`, a subscription
id, a GAM section as `select <section>` for that one process), so nothing on disk changes. A shell pane gets a small
overlay kubeconfig that holds names only and no credential, first in `KUBECONFIG`, in a file of mode 0600 below
`run/kube/` (removed when the pane is gone; a sweep removes the ones whose pane no longer exists). A profile can give Azure and GAM7 a
configuration folder of their own below `integrations/`. The tools' own files change only through a confirmed
`Local` verb that names the file. The tools may still write their own state (a token refresh, a `set-context` you type
yourself), and a start file of your shell can export `KUBECONFIG` again and replace the overlay.

**Approval.** A verb that changes something or opens a session returns a card with the exact argument list, the target
and a hash; `confirm` repeats the hash, the core resolves the request again, hashes again, refuses any difference and
runs it once. A card waits 120 s, three wrong hashes drop it and at most eight wait. The window is the only decider:
nothing of it exists on the `wsh` socket, nothing is remembered, and the AI chat reaches no integration. A context
that a profile marks as production shows its name and the word production on the card and needs a second press, for
every verb that has a card; a press within half a second of the card appearing or of the first press is ignored.

**Terminal blocks.** Exec in a pod, logs, node messages, the Talos dashboard and `az login` are a pane whose program
is the tool. A failing start is held on screen until you press Enter, so the error can be read. The pane gets no
`HOLZWAVE_*` variable and is not restored on restart: it is shown as a plain shell with one line that says what it ran.

**View model.** A table (at most 64 columns and 2000 rows), a key-value list or a text. Every string from a cluster or
an account is cleaned of control, direction, zero-width and format characters, cut, and shown as text only; the page
removes the same characters again and cuts a cell at 240 characters. Stderr is shown as one cleaned and cut line and
never logged.

**What the integrations never do:** print a raw kubeconfig, pass a verbose flag, ask for an access token, export a
credential, emit a `save` that rewrites a GAM configuration, or offer anything to the AI chat.

**As built.**

| Part | Where | What |
|---|---|---|
| Names and arguments | `core/src/integrate/args.rs` | every name that can reach an argument is a type with one validating constructor; a flag that carries a name is built in the equals form |
| Detection | `detect.rs` | the order above, the `PATH` of a child, a look again right before a run |
| Runner | `run.rs` | argument list, scrubbed environment, capped output, timeout, process-group kill, four slots; compiled out on iOS and Android and disabled in a Flatpak build |
| Views | `view.rs` | the view model, its caps and the one place text from a cluster is cleaned |
| Profiles | `profiles.rs` | `integrations.json`, `integration_local.json`, the secret variables in the keychain |
| Context | `context.rs` | the selection of a pane, its application to a run, the overlay and the isolated folders |
| Approval | `ticket.rs` | the card, the ticket, the hash, the production guard; the one place an `Approved` value is made |
| Service | `service.rs` | the registry, reads with single flight, faults, prepare and confirm, terminal launches, the cached status |
| Tools | `kube.rs`, `talos.rs`, `azure.rs`, `gam.rs` | the argument lists and parsers of one tool each, and nothing else |
| App shell | `app/src-tauri/src/integrate_host.rs` | builds the service from the real machine, answers the `integration_*` commands, sends the event `integration` |
| Page | `app/src/integrate*.ts`, `integrate.css` | the block, the Context section and status segments, the card, the profiles dialog, the palette entries |

The flows are three. A **read** is `integration_run`: the request is checked against the closed catalogue, the selection of
the pane is resolved with its profile, the tool is found, the argument list is built, the environment is made, the runner
runs it and the answer becomes a model or a fault. A **change** is `integration_prepare` (a card, nothing started) and then
`integration_confirm(ticket, hash)`. A **terminal** is `integration_launch` (a verb that only reads) or the pane that a
confirmed card opens. The **status** of a tool (the Context section, the status segments, the chip of a pane) is a cached
value read by one background thread, `holzwave-integration-status`, started by the first ask; the core has no timer, so an
idle or hidden app starts no process. A tool is due after 30 s (Kubernetes) or 60 s (the others), a failed read doubles the
wait up to 300 s, and a signed-out or missing tool is a state and does not back off.

User widgets are data, not code: a JSON document validated against a schema, rendered by the webview from a fixed
set of widget kinds. There is no arbitrary script execution. The loader (`core/src/widgets.rs`) validates every file field
by field, and `docs/widget.schema.json` (JSON Schema, draft 2020-12) describes the same documents for an editor; a test,
`core/tests/widgets_schema.rs`, gives the schema and the loader the same documents and fails when they differ.

## 9a. Files and previews (Phase 4)

A **block pane** is a pane that runs no process: its link is a `NullLink` and the page draws its content (a file browser
or the preview of one file) over the empty terminal. It has a header, a place in the split tree, drag and close like any
pane, and `pane_blocks.json` remembers what each shows. `core/src/files.rs` has one `FileSource` interface with two
implementations: this computer, and the files of a saved SSH connection over SFTP. The files, the commands and the shell
of a saved SSH connection share its one login (`core/src/pool.rs`): a file read does not wait behind the terminal's output
because every consumer of the connection drains its channel at once, and the shell's backlog is capped at 16 MiB, past which
the shell is closed and says why while the other channels go on. Reads are bounded, writes are atomic (written aside, renamed over).

Images, video, audio and PDFs are **not** loaded into the page. `file_grant` makes a token (128 random bits) for one file,
and the `holzfile` URI scheme serves exactly that file from `holzfile://localhost/<token>` (`http://holzfile.localhost/`
on Windows and Android) with byte ranges, so a video starts and seeks at once and a large file costs no memory. There is no
path in the URL, so the page cannot ask for a file it was not given; a grant ends when its preview goes
(`file_revoke`), at most 256 live at once. Responses say `nosniff`, `no-store` and `Content-Security-Policy: sandbox`.

A refused grant answers the core's own sentence, which is for people and is not a contract. A page that wants to say why asks
`file_grant_why` for the same file after the refusal: the answer is one member, `code`, and its values never change
(`locked`: the secrets are locked and the source needs one; `not_found`: nothing is at the path on this computer; `denied`:
the person's own account may not read it; `kind`: not an image, video, audio or PDF, or a folder; `other`: anything else,
including every absence on a saved connection, which the core does not yet report apart from its other errors). The
command grants nothing, reads at most the first bytes the grant reads, and repeats nothing of what it was sent.

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
| Synced settings | read by the server | end-to-end encryption, the server sees ciphertext and sizes; a holzWave server as the target checks only the header and the size of a file, holds no key and logs no name or byte of one; neither target can be stopped from serving an older valid file (no rollback protection) |
| The PTY | input injected by page content | strict CSP, no untrusted HTML in the main page, previews sandboxed |
| Layout / settings files | corrupt or hostile file crashes the app | validated on load, fall back to a fresh state |
| Paste | control sequences run commands | bracketed paste; warn on multi-line paste to a shell that does not support it |
| A pooled SSH session | one login shared by a pane, the files and the commands: a command runner learns a host key, a wrong password is tried again and again, a stopped pane stalls the others, or idle logins pile up | a command runner (a monitor, the agent) never learns a host key and a changed key is refused for everyone; a background login waits 15 s after a refused login and 3 s after a lost connection, while a person's own open is never held back; the shell's backlog is capped at 16 MiB and the shell is closed past it; a session unused for 120 s is closed, and at most 16 connections are kept |
| A login prompt | the server of a connection asks a question (a one-time code, a PAM prompt) and writes the text itself, so it can imitate a holzWave question, put markup or hidden characters in it, ask without end, or be answered by a background poller that locks an account; an answer ends up in a log, a file or the keychain | the server's text is untrusted: the core removes control, direction and zero-width characters and bounds it (60, 300 and 200 characters, at most 5 questions a round, 3 rounds, 120 s), and the page sets it as text only under the fixed sentence "This text comes from the server, not from holzWave."; the title and the host of the dialog come from the person's own saved connection by its id, never from the server; an answer is moved into that one login and never stored, logged or part of a `Debug` text, and a connection of this kind cannot hold a saved secret; a background opener (a monitor, a command) never asks and never waits behind a person, so only a pane or a file browser raises the dialog, once for the pane, the files and the commands. Tested against an in-process server; not yet against a real one-time-code host (`docs/test-plan.md`) |
| wsh over an SSH tunnel | a process on a remote host asks the app for more than a preview, or another user of that host reaches the socket | one Unix socket per remote pane is forwarded in reverse over the pooled login; `sshd` creates it with an owner-only mode and its path holds 128 random bits; a per-pane token is checked first; the protocol crate's remote scope accepts `ping`, `view`, `edit` and `setmeta` only and takes the pane from the forward, never from the request; at most 4 connections per pane, 1 MiB per connection, 10 s of silence, 30 s of life and 20 requests per 10 s; opt-in per connection, no TCP fallback, a new socket path for every start of a shell, and `wsh` is put on a host only by one explicit act of the person |
| Everything the app is | telemetry, an update channel or an account arrives through a dependency or a plugin (DESK-26) | none exists; `core/tests/project_policy.rs` pins the dependency lists and the closure of the app, scans first-party sources and the page for network APIs and fails on a CI workflow |
| Provider keys | read from a file, a log or the page, or sent to another host than the one they were saved for | the OS keychain only, under the account `ai:<id>` together with the address they were saved for (a changed address needs the key again); write-only, because no command returns a key; every type that holds one hides it in `Debug`; never synced and never in `ai.json` |
| Model output | a reply runs code or loads something in the page | untrusted text; it is rendered by the core's sanitiser and copied through the page's own allow-list; no image or media element ever; a link with a query string or user information is inert |
| Agent actions | a tool call does something the person did not see or approve | typed tools; each call shows its exact fields and a hash; only the window decides (`ai_decide`, and `ai_undo` for a file change), by repeating the card's hash, and the core checks again when the call runs; nothing is remembered; after untrusted text entered a turn every side-effect tool needs a fresh card; a deny list protects the app's own data, configuration and socket folders, shell start files, `~/.ssh`, `.git/hooks` and `.env` files; a command runs in a separate process with a scrubbed environment and is never typed into a pane; `wsh` and the local socket can neither approve nor consent |
| AI traffic | a request leaves that nobody asked for, or goes to another place than the person chose | one HTTP chokepoint (`core/src/ai/http.rs`): https only (a loopback address, and the local network by an opt-in), no redirects, the resolved address pinned for a destination on this computer or the local network, bounded sizes and times, error text without URLs; nothing is requested before Send, and a name lookup happens only on an act that needs the destination; `Local only` refuses every destination that leaves the computer; the page never talks to a provider and the content security policy is unchanged |
| Integration tools | a tool is started with more than the person decided: a name from a cluster, an account or a synced file becomes an option, a shell string or an environment variable of a loader | every child is an argument list started by the one runner, never a shell; every name is a validated type and a flag that carries one uses the equals form; a synced profile cannot set a loader, interpreter, program or app variable, and a file that breaks a rule is set aside whole; a verb that changes something is a card with a hash that only the window answers, once; the AI chat, `wsh` and the socket reach no integration. Tested by generated corpora against fakes (section 12), not against real tools |
| Integration output | text from a cluster or an account spoofs the window, the card or a terminal (escape sequences, right-to-left marks, markup, huge cells) | cleaned once in the core, capped, cleaned again by the page and drawn as text only; the card spells control characters as escapes; a row whose name fails validation has no action |
| Integration secrets | a secret variable of a profile reaches a file, a log, a card, a `Debug` text or another run | in the keychain as `int:<profile>:<NAME>` (at most 1 KiB), write-only, read for one spawn and wiped, never in `integrations.json`; a canary sweep over every output of the service finds the secret only in the environment of the runs that were given it |
| The browser sign-in | a guessed password, a stolen cookie, a forged request from another site, a session that never ends | Argon2id behind a gate of two running derivations; a password needs a six-digit code of an authenticator app; five wrong attempts per name and twenty per address lock the password path for 15 minutes; passkeys and single sign-on are not throttled; the session is 43 base64url characters whose hash alone is stored, idle 30 minutes, absolute 24 hours, with a recent sign-in required to change a credential; every route checks Host, Origin, content type, a 64 KiB body, the session and a derived CSRF token before it runs (`server/tests/auth.rs`, `server/tests/edge.rs`, `server/tests/origin.rs`) |
| The browser cookies | script reads a session, another site sends it, a sub-domain replaces it | `__Host-hw_session` is `Secure; HttpOnly; SameSite=Strict; Path=/` with no `Domain`; the single sign-on hop uses `__Host-hw_oidc` with `SameSite=Lax` and ten minutes, because the identity provider's redirect is a cross-site navigation, and that cookie is single use and bound to the browser; the half-finished enrolment uses `__Host-hw_pending` (`server/tests/edge.rs`) |
| The browser page | an injected script, a framed page, a leaked address | a fixed header set on every response (a strict content security policy with `connect-src 'self'` and the page's own WebSocket, HSTS for a year, `no-referrer`, `nosniff`, `same-origin` opener and resource policies, a permissions policy, `no-store`), and no `access-control-*` header ever; the setup token and an invitation travel in the address fragment, which a browser never sends to a server (`server/tests/prohibitions.rs`) |
| One container, many people | a person reads, signals or inspects another person or the server | no process has user id 0; the supervisor and the helper run as 10001 and the helper alone carries `cap_setuid,cap_setgid,cap_chown,cap_kill=p`; a worker, its shells and their children run as a user id of the range 20000 to 29999 with no capability, `no_new_privs`, four descriptors and a fixed environment; homes are mode 0700 under a 0711 base; the server directory, the database and the audit files are readable by the server user only (`server/tests/isolation.rs` in the container, forty tests) |
| The server key | a key in a log, an image or the data volume, or a server that makes one silently | the server never makes a key: `holzwave-server keygen` writes a file with mode 0600 that is never overwritten, the reader refuses a link, a loose mode and a wrong shape, and the purpose keys (sealed values, CSRF, ACME account) come from it by HKDF (`server/tests/config.rs`) |
| Saved passwords on the server | the operator or another person reads a person's secrets | one file per person, XChaCha20-Poly1305 under a random data key that is wrapped by up to four kinds of key slot (password, passphrase, passkey PRF, recovery code); the key is in the worker's memory only while a session of the person is open and leaves it when the last one ends, with one audit line; an administrator can reset a person's secrets and can never read them (`server/tests/secretstore.rs`, `server/tests/unlock.rs`, `server/tests/canary.rs`) |
| The audit log | a secret or a typed name in a log, a log that can be rewritten | one JSON line per security event in a daily file with mode 0600, opened append-only; the action is a closed list and no member holds text a person typed; the server does not rotate or delete the files (`server/tests/audit.rs`, `server/tests/audit_once.rs`) |
| Uploads and downloads | a file written outside the home, over another file, or a download of an unlisted file | the person's own worker writes into `<home>/uploads` through a directory descriptor, never overwriting and never following a link; a download is a 32-character token granted by the core for an image, video, audio or PDF file, served with a `sandbox` content security policy (`server/tests/files.rs`) |
| Device tokens | a token read from a file, a log or the page, guessed, replayed by another person, or kept alive forever | the token is `hwd_` and 256 random bits, only in the OS keychain of the device (account `server:token`) and only as a SHA-256 on the server; its type prints a marker in `Debug` and cannot be cloned or serialised; it travels in one `Authorization` header to the origin it was issued by; it ends after `devices.idle` (90 days by default) without use and a revocation closes its sockets at once (4401); it is not bound to the device and not rotated (section 16), so a stolen token works until it is revoked or runs out (`server/tests/device_tokens.rs`, `server/tests/device_client.rs`, the unit tests of `core/src/server_link.rs`) |
| The device grant | codes guessed, polls flooded, a person phished into approving another's request | a 256-bit device code and an eight-letter user code, hashes only, ten minutes of life, a poll paced at 5 seconds and slowed by 5 more on each early poll, five wrong codes in ten minutes lock the account and the address for ten minutes, 5 open requests per address and 200 in all, 20 devices per person, approval only with a recent sign-in; the page shows the device, its platform and the address of the request and says to approve only what you started, which is all that stands against phishing (`server/tests/device_grant.rs`) |
| Cookies and bearer tokens | a cookie used for a device route, or a token for a page route | a route is one of the kinds `Device`, `DeviceOpen` or a session kind, and a device route never reads a cookie while a session route never reads `Authorization`; the independent route list of the prohibitions test walks every route in both switch states (`server/tests/device_access.rs`, `server/tests/prohibitions.rs`) |
| A device feature that is off | a probe learns that the feature exists, or an old token keeps using it | every path of a feature that is off answers as a path that does not exist, before any credential is read; turning remote sessions off closes every device socket with 1001 (`server/tests/device_access.rs`, `server/tests/remote_sessions.rs`) |
| Sessions on the server | one person attaches to another's session, a flood fills the worker, terminal bytes reach a log | a session number is the worker's own and another person's answers `not_found` like one that never existed; requests, input and output are bounded and a reader that falls behind gets a fresh snapshot; one writer per session, input of a watcher is dropped; no terminal byte or token reaches a log, the audit files or the database (`server/tests/remote_sessions.rs`, `server/tests/session_host.rs`) |
| Sealed deployment | an operator weakens the pod and the server then runs without isolation | the server refuses to start when the helper lacks its capabilities, and never runs with a reduced set; `allowPrivilegeEscalation: false` and `no-new-privileges` are absent on purpose because they stop file capabilities; the checker `scripts/web-deploy-check.mjs` judges Compose and the chart (`sh scripts/web-container-test.sh image`) |

### AI chat

**What leaves the computer.** The text you type, the context you ticked after you shared that pane, and the results of the
tool calls you allowed: nothing else, and nothing before you press Send. A key is sent only to the address it was saved for,
in one header.

**Stored chats.** A chat is one file in `chats/`, readable by the owner only and written atomically at message boundaries.
Terminal context is stored as a marker unless `ai_persist_context` says text, and turning that setting off rewrites the
existing files without it. Deleting a chat is an unlink, not a secure overwrite, and a damaged file is moved aside as `.bad`.

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

### Integrations

**What runs.** Your own `kubectl`, `talosctl`, `az` and GAM7, by an argument list built from a closed catalogue, with the
environment of section 9, and never through a shell (one constant string picks bash or sh inside a pod). Nothing starts when
the app opens; a read starts when a block that is shown or a status that the page asks for needs it, and a hidden window
starts none. The integrations make no connection of their own: the tools do, and what they send is theirs, except that the
Azure runs holzWave starts carry `AZURE_CORE_COLLECT_TELEMETRY=false`.

**What is cleaned.** A name is validated before it can be an argument (a leading dash, control, direction and format
characters are refused, and a refusal never quotes the input). Cell text, fault text and card text are cleaned and capped in
the core and cleaned again in the page. A kubeconfig is never shown as text and no type of the integrations has a member for
a token or a credential. A secret never reaches a card, a fault, a `Debug` text or a log.

**Who can decide.** Only the window, by a press that a person made on a card with the exact argument list and a hash; the
core resolves the request again at the press and runs it once. On a production context the button needs a second press.
Nothing of this exists on the `wsh` socket and the AI chat has no integration tool.

**Accepted residuals.** The value of a secret variable is in the environment of the tool it was given to, and a process of
the same account can read it (for example `ps eww`) while that tool runs. A credential plugin of a tool may refresh a token
in your kubeconfig on its own. A start file of your shell can export `KUBECONFIG` again and replace the overlay of a pane, so
the chip says what holzWave set and not what the shell then did. The network and telemetry behaviour of the tools is theirs.
The production guard follows the selection of the pane the request comes from, so a switch of the tool's own default toward a
production context from a pane that is not on one shows that target on its card and needs one press. The overlay of a shell pane
is named for the pane it was opened from, so two shells opened from one pane share a file (a known defect, recorded by an
ignored test). A compromised page could press a button on a card; that is bounded by the page controls and the hash, as for the AI chat. A
name that is made of blank-looking letters outside the format category of Unicode (such as U+3164) is accepted; nothing
executes it, and the card spells the characters it shows. The Flatpak build and mobile run no integration, so they have no
such exposure.

### wsh over an SSH tunnel

**What is forwarded.** One Unix socket for each remote pane of a connection that has `wsh` switched on (it is off until the person turns it on). The app asks the `sshd` of the host for a reverse forward of `/tmp/holzwave-` plus 32 random hex characters (128 bits) plus `.sock` on the connection's pooled login. `sshd` creates the socket with its owner-only mode (`StreamLocalBindMask`, `0177` by default), so another user of that host can neither open it nor guess its name. A host that does not allow Unix socket forwarding gets no tunnel and its pane stays a plain shell with one line that says why: there is no fallback to a TCP port, because a port on a host is open to every user of it. The shell learns the socket, a per-pane token and the pane number from its environment, which the app sets with one constant start line and never types into the shell, so no secret reaches the history file of the host.

**What a remote process can ask.** `ping`, `view`, `edit` and the title of `setmeta`, and nothing else. `ai`, the history commands, consent, approval and every command that decides anything are refused with `not available over a remote connection` before a body is read, by the protocol crate itself; a command added to the protocol does not compile until it has a decision for this scope, and the guard that no command can decide, approve, confirm or grant covers it. The pane and the scope come from the forward and the per-pane token: the `pane` member of a request is ignored. The token is checked in constant time before anything else is looked at.

**Limits.** At most 4 forwarded connections are open at once for a pane (a fifth is refused), each may send 1 MiB and is ended after 10 s of silence or 30 s of life, and a pane may send 20 requests per 10 s (then the answer is `busy`). The bytes of a forwarded connection reach the code that answers through a queue of fixed size and the connection is closed when it is full, so a slow or hostile peer cannot hold up the other channels of the login. The forward ends when the pane closes or its login is replaced, and is asked for again after a reconnect, on a new path (see below).

**What is not defended.** A process of the same account on that host holds what the pane's shell holds: it can read the token from the environment, open the socket, ask for previews of any file that account can read and rename the pane, at the rate above. A compromised host can ask; it can never decide, consent or approve for the person, and it can never reach the AI or the history. The administrator of the host can do all of this for every account on it. WSL and Docker panes have no SSH session to forward over and no tunnel, and a host that is not Linux has no build of `wsh` to install.

**Paths, and the file `sshd` leaves.** `sshd` refuses a forward for a path that already exists and does not unlink one unless it is told to (`StreamLocalBindUnlink`, a setting of the host that the app cannot make). The OpenSSH of the live check below also leaves the socket file behind when the forward is cancelled and when the login ends: an empty, owner-only socket in `/tmp` with nothing listening on it. So the first start of a pane's shell asks for the path its tunnel was made with, and every start after it (a reconnect, or Enter after a failure) asks for a new path of the same form, with 128 fresh random bits, and the new shell's environment names it; the token and the pane stay the same. The files that stay are harmless and are left to the host's own cleaning of its temporary folder.

**What answers on the app's side.** The sink of a pane's tunnel is made for that pane and answers `ping`, `setmeta title`, `view` and `edit` for it. `view` and `edit` need an absolute path that the pane's own saved connection finds to be a regular file over SFTP; the preview or the editor opens beside the pane with that connection as its source, and a path of this computer is never read. When a pane starts, the app records where its saved connection points (the host, the port and the user). A connection that is edited to another host afterwards is refused, with one fixed sentence, for every request of that pane and for the install below, because the process that asks is on the old host and a path would otherwise be resolved on the new one.

**How `wsh` finds the socket, and how it gets onto a host.** On this computer `wsh` finds the app through `HOLZWAVE_IPC` (the folder of its socket). On a host reached over SSH the start line sets `HOLZWAVE_SOCKET` (the path of the forwarded socket) with the token and the pane, and not `HOLZWAVE_IPC`: `wsh` prefers `HOLZWAVE_SOCKET` when it is set (Unix only), and the history hooks, which test `HOLZWAVE_IPC`, stay off on that host. `wsh version` prints the version without an app. The binary gets onto a host by one explicit act, the palette action `Install wsh on this host` for the focused pane of a saved SSH connection: the app asks the host `uname -sm` (a fixed command), accepts Linux on `x86_64` or `aarch64` only, skips the upload when `wsh version` there already says the app's version, and otherwise uploads the static build that the release scripts bundled (read from two fixed names in the resource folder, never downloaded and never built on the host) over the connection's pooled SFTP to `~/.holzwave/bin/wsh`, with the folders at mode 0700 and the file at mode 0755, written aside and swapped in so that an old file is never removed before the new one is in place, runs `wsh version` there to check it, and says the result as a line on the pane. Nothing is installed at connect or on a timer, nothing but that file and its two folders is created, and no startup file of the account is changed (the start line appends `~/.holzwave/bin` to `PATH` itself). A host that is not Linux, another processor, a build of the app without the bundled files, a host without SFTP and a server that refuses the upload each get one sentence on the pane and keep running; a failed upload leaves no temporary file.

**What was run against a real OpenSSH.** `sh scripts/ssh-loopback-check.sh` starts a throwaway `sshd` on loopback that allows Unix socket forwarding, builds `wsh`, saves a connection to it with `wsh` ticked in an app core, starts a pane and types the real `wsh` into the real remote shell: `ping`, `setmeta title`, `view` and `edit` of a file of that host work through the forward; `ai`, `hist add` and a wrong token are refused; the socket exists with an owner-only mode while the pane runs and nothing listens on it after the pane closed. That is the build of the computer that runs the check, started by its absolute path, so nothing is installed into a home. What it does not show is a Linux host, another processor, the static Linux builds through the tunnel (they are run in a container by the release scripts) and a server of another make; the owner's rows for a real Linux host are in the test plan.
### The platform seam (Phase 2.2)

Windows and Linux are written and cross-checked from a Mac, and the author has run them on Windows and Linux machines
([test-plan.md](test-plan.md) Part 4). What exists, as built:

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
  [design.md](design.md) section 7. The Windows installer and the macOS app are not signed (the macOS app only ad hoc); the
  Linux packages can carry the owner's minisign signatures and a signed `SHA256SUMS`, and no release has been signed yet
  ([signing.md](signing.md)).
- **The cross-check ladder, from the Mac.** `scripts/cross-check.sh` compiles every test target of the core for
  `x86_64-pc-windows-gnu` and `x86_64-unknown-linux-gnu` and the app for Windows (GNU ABI: Zig cannot build the MSVC
  library on a Mac, so the MSVC build exists only on a Windows host). `scripts/linux-container-test.sh` runs the core and
  app tests in an Ubuntu container, and `scripts/linux-container-bundle.sh` builds and inspects the deb and rpm there.
  On their own they prove that it compiles and that the Unix half passes on Linux; a screen, an input method, a GPU or
  ConPTY are what the author's run on real machines covers.

- **The integrations on the platforms.** macOS: built, and run against fake tools and recorded output; the research Mac had
  `kubectl`, `az` and GAM7 and no `talosctl`. Linux: the whole core suite ran once in an Ubuntu container (arm64) with one
  failure outside the integrations (an SSH test); the Linux directory list, Snap, Linuxbrew and a desktop started from a
  launcher were not covered. Windows: the rules (the `PATHEXT` search, the batch-file argument charset of `az.cmd`, the `;`
  separator of `KUBECONFIG`, the refusal of UNC and device paths) are tested with injected facts on the Mac; the five tests that
  use the real system are in `core/tests/integrate_windows.rs`, have never been compiled for Windows and never run, and the
  Windows and Linux cross-check did not run for this part. Mobile: the code that starts a process is compiled out and the
  answer is `Unsupported`; the compile for Android runs in `sh scripts/android-check.sh` and none was made for iOS. Flatpak: the runner is built disabled, so
  every tool reads `unsupported` and no process starts; no real sandbox ran. Rows INT-W1 to INT-W4 and INT-L1 of
  [test-plan.md](test-plan.md) are the owner's.

### Android

Android is built for arm64 only and was run on arm64 emulators of Android 12, the newest Android and an image with 16 KB memory
pages, never on a phone or a tablet ([test-plan.md](test-plan.md) Part 13 is the list of what only a phone shows; the build, the
pins and each emulator run are in [build-android.md](build-android.md)). The core is the same one; what Android adds is a thin set of
layers around it:

- **The seams.** `Host` names the platform and the private folders (`AndroidHost`), `SecretStore` is the vault below, and `FilePicker`
  is the one-file hand-over through the system's document dialog. The host crate decides every name, path and size of a hand-over; the
  Kotlin side shows the dialog and copies bytes.
- **The in-app plugin.** One first-party Kotlin class (`HolzwavePlugin`) is registered with Tauri's own plugin builder inside the app
  crate; no `tauri-plugin-*` crate is used and the page cannot call it. It carries the Keystore calls, the foreground service, the two
  dialogs, the clipboard and the setting of the app switcher picture. Rust calls it from worker threads, never from the window's thread.
- **The vault.** `core/src/vault.rs` is pure Rust over a `Keystore` trait: one XChaCha20-Poly1305 file (`vault.bin` in the app's data
  folder) whose data key is wrapped by an AES-256-GCM key that never leaves the Android Keystore, with the states new, locked, unlocked
  and lost and the protections `auth` (the screen lock or a strong biometric for every unwrap, through the system prompt) and `plain` (a
  device with no screen lock). It locks five minutes after the page was hidden, by a clock that counts the time the phone slept.
- **The foreground service.** A service of the type `specialUse` holds the process while a shell or a connection is alive, with one
  notification (the counts only) and a `Stop all` action; `AppCore::stop_all` ends every pane and keeps the windows.
- **The phone view.** The page applies one pure transform to the core's view (`phoneview.ts`): the focused pane alone at full size. The
  core's layout is untouched, so a layout saved on a desktop is the same layout on a phone.
- **The layout classes.** The page decides phone, tablet or desktop from the window width and the screen's short side, and only for
  Android; every other platform is the desktop class.

Accepted limits, said where a reader needs them: the shell runs as the same user as the app and can read the vault file, which holds
ciphertext only (its key never leaves the Keystore), so the folder the shell starts in is a guard against accidents and not a
security boundary; screenshots and screen recordings are not blocked; while the vault is protected, the app switcher picture is hidden
by a system call whenever the app is not in front, on Android 13 and later, and on Android 12 and 12L nothing hides it and it may
show terminal content, which is a known limit; Android may end the local shells of a background app (SSH connections live in the
app's own process and are not affected).

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
  measured again in Phase 2. These two ceilings are coarse limits from that earlier stage; they stay as they are, and the guard below is
  what judges a change.
- **Baseline and guard.** The numbers of the machine are recorded in `perf/baseline-<os>-<arch>.json`, and
  `sh scripts/perf-check.sh [core|server|web|container|all]` measures again in release mode and compares each metric with that
  file. It prints `PERF OK`, or `PERF WORSE` and exits 1 when a metric is clearly worse, and exits 2 when it cannot judge (no
  baseline, a measurement that failed, a metric the file lacks). It sets no fixed target: the judge is the number recorded on the
  same machine. Four groups are measured. *Core* (`core/tests/perf.rs`): parse and pane throughput, update plus encode of a
  160x50 frame, key to frame (median and 99th percentile at the pane's sink) and pane start, and the memory of a pane empty, with
  a full scrollback and with pictures up to the default cap (`core.memory_pane_images_mib`: four engines fed twice the default
  64 MiB in distinct 1 MiB pictures, the growth of the resident set beyond an empty pane, per pane). *Server*
  (`server/tests/perf.rs`): key to frame on a real TLS WebSocket, reattach, and memory per pane with 50 panes in one process.
  *Web* (`app/tests/web/perf.mjs`): key to paint and reload to first paint in Chromium and WebKit against the real server.
  *Container*: the core and server groups in the Linux container (`sh scripts/linux-container-test.sh perf`), which does not judge
  the 99th percentiles because a Docker VM moves them by a factor of five between sessions. Memory is the resident set of a fresh
  process per run, so it is an upper bound that includes the test's own buffers. The tolerances are in `scripts/perf-compare.mjs`:
  a time is worse above 1.5 times the recorded value plus 0.1 ms, a throughput below half of it, a memory above 1.5 times it plus
  4 MiB. They are wide on purpose, because a single run moves by 10 to 20 percent on its own: in three sessions on the same Mac,
  four metrics (key to frame p50 of the core, pane start, key to frame p99 of the server and key to paint p50 in Chromium) came out
  10 to 19 percent above the numbers recorded before the work, and an alternating run of the code from before the work against the
  current code on the same machine measured both the same (the engine's construction plus first update takes 0.053 ms in both),
  so the difference is the machine's spread and not the code. To record again, on purpose and with the reason in the commit message:
  `sh scripts/perf-record.sh all --replace` (and `container --replace`). The numbers before the pictures and links were built are
  kept in `perf/before-<os>-<arch>.json`; the table is copied from those files and the files that the guard now compares with, one
  recording each, on 2026-11-02 and 2026-11-04. The pictures cost nothing a frame without pictures pays: every metric of the
  time and throughput kind is within 10 percent of its value before, and the one metric that is new, the memory of a pane that
  holds pictures up to the default cap, is about 69 MiB on the Mac and 66 MiB in the container, beyond an empty pane. A single
  picture of 64 MiB costs several times its size while it arrives (289 MiB measured at the default cap), which this figure does not show.

  Apple M1 Max, 10 cores, macOS (`perf/before-darwin-arm64.json` and `perf/baseline-darwin-arm64.json`):

  | Metric | Unit | Before | After | After / before |
  |---|---|---|---|---|
  | `core.key_to_frame_p50_ms` | ms | 0.134 | 0.135 | 1.00 |
  | `core.key_to_frame_p99_ms` | ms | 0.949 | 0.667 | 0.70 |
  | `core.memory_pane_empty_mib` | mib | 0.922 | 0.928 | 1.01 |
  | `core.memory_pane_images_mib` | mib | none | 68.73 | new |
  | `core.memory_pane_scrollback_mib` | mib | 13.80 | 13.80 | 1.00 |
  | `core.pane_output_mib_s` | mib_s | 43.11 | 43.98 | 1.02 |
  | `core.pane_start_ms` | ms | 7.409 | 7.587 | 1.02 |
  | `core.parse_mib_s` | mib_s | 61.34 | 60.62 | 0.99 |
  | `core.update_encode_ms` | ms | 0.445 | 0.457 | 1.03 |
  | `server.key_to_frame_p50_ms` | ms | 0.598 | 0.537 | 0.90 |
  | `server.key_to_frame_p99_ms` | ms | 1.204 | 1.128 | 0.94 |
  | `server.memory_pane_empty_mib` | mib | 0.292 | 0.298 | 1.02 |
  | `server.memory_pane_scrollback_mib` | mib | 4.424 | 4.424 | 1.00 |
  | `server.reattach_ms` | ms | 2.929 | 2.827 | 0.97 |
  | `web.chromium.key_to_paint_p50_ms` | ms | 2.800 | 2.900 | 1.04 |
  | `web.chromium.key_to_paint_p99_ms` | ms | 15.70 | 16.70 | 1.06 |
  | `web.chromium.reattach_paint_ms` | ms | 114.5 | 113.0 | 0.99 |
  | `web.webkit.key_to_paint_p50_ms` | ms | 13.00 | 13.00 | 1.00 |
  | `web.webkit.key_to_paint_p99_ms` | ms | 23.00 | 23.00 | 1.00 |
  | `web.webkit.reattach_paint_ms` | ms | 158.5 | 156.5 | 0.99 |

  Linux container (Ubuntu, aarch64, 10 CPUs of a Docker VM on that Mac; `perf/before-linux-aarch64.json` and
  `perf/baseline-linux-aarch64.json`). It has no browser, so no `web.*` rows:

  | Metric | Unit | Before | After | After / before |
  |---|---|---|---|---|
  | `core.key_to_frame_p50_ms` | ms | 0.399 | 0.364 | 0.91 |
  | `core.key_to_frame_p99_ms` | ms | 1.100 | 0.948 | 0.86 |
  | `core.memory_pane_empty_mib` | mib | 0.685 | 0.661 | 0.97 |
  | `core.memory_pane_images_mib` | mib | none | 66.10 | new |
  | `core.memory_pane_scrollback_mib` | mib | 12.27 | 12.26 | 1.00 |
  | `core.pane_output_mib_s` | mib_s | 52.77 | 51.22 | 0.97 |
  | `core.pane_start_ms` | ms | 3.361 | 3.209 | 0.95 |
  | `core.parse_mib_s` | mib_s | 44.90 | 43.34 | 0.97 |
  | `core.update_encode_ms` | ms | 0.521 | 0.557 | 1.07 |
  | `server.key_to_frame_p50_ms` | ms | 1.432 | 1.275 | 0.89 |
  | `server.key_to_frame_p99_ms` | ms | 2.913 | 2.675 | 0.92 |
  | `server.memory_pane_empty_mib` | mib | 2.036 | 2.033 | 1.00 |
  | `server.memory_pane_scrollback_mib` | mib | 4.041 | 4.013 | 0.99 |
  | `server.reattach_ms` | ms | 2.266 | 2.241 | 0.99 |

  What these numbers do not cover: Windows was not measured; the desktop window's key to glass was not measured (the core's
  numbers end at the pane's sink, the web numbers end in a browser against the real server); the browsers ran headless on one Mac
  and a key to paint includes the wait for the next animation frame; the server's numbers come from that Mac and from an aarch64
  container, a stand-in for x86_64 hardware; and every number is one machine at one time. No metric is carried as a cost of the
  pictures or links.
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
- **Native integrations:** the quick set of Part 1 of [test-plan.md](test-plan.md) runs the tool modules, the two JSON
  contracts, the service, the status worker and the end-to-end files against fake programs (POSIX scripts) and recorded
  fixtures, and a release gate of generated corpora runs on top of it. Measured on an Apple silicon Mac on 2026-10-22: the
  name-injection corpus has 38 classes over 22 verbs and 44 argument slots (7296 requests, 5814 refused, 1482 accepted, each
  accepted list equal to the baseline with the name in its place, no refused request started a process); the hostile-output
  corpus has 4134 documents in 113 classes and 3144 distinct models, all accepted by the schema check; the canary sweep checks
  52 outputs for 5 canary values; the approval-bypass test makes 49 lane attempts and 24 ticket attempts, all refused with no
  process started; the page gate puts 26 hostile shapes through the block on WebKit and Chromium. The counts are printed by the
  tests and each has a minimum asserted, so a corpus cannot shrink unnoticed. These are tests against fakes and fixtures: no
  real tool was in the loop, the fixtures for Talos, the GAM7 users and the Azure resource groups are written from the tools'
  documents and not recorded, and the gated real-tool tests (`integrate_real`) skip unless `HW_INTEGRATION_REAL=1`. The human
  rows are Part 9 of [test-plan.md](test-plan.md); how many of them have passed is not measured yet, the Result column says.
- **Robustness:** corrupt `layout.json`, closing panes mid-output, and resizing during a flood are explicit tests,
  as are hostile `settings.json`, `session.json` and theme files (size, type, range and name limits).
- **What no suite here can see** (real window chrome, an input method, a physical layout, the native menu, fonts, the
  process table after quit) is one human checklist, [test-plan.md](test-plan.md), with a per-platform list for
  Windows and Linux (D-22).

## 13. Build and release

- Toolchain: Rust stable, Zig 0.17.1 for `libghostty-vt` (the pinned Ghostty commit, 521cf2c, requires it in `build.zig.zon`), Node and `tsc` for
  the frontend. `libghostty-rs` is pinned by git revision. On a Mac the Swift toolchain (Xcode or the Command Line Tools)
  builds the Apple bridge crate with the workspace; the Foundation Models code is compiled only when the installed SDK has
  the framework (Xcode 26 or newer), and an older SDK builds the app without the model ([building.md](building.md)).
- Versions `0.0.1-alpha1` to `0.0.1-alpha3`, `0.0.1-beta1`, `0.0.1-beta2` (now), then `0.0.1`. Alpha and beta are GitHub prereleases.
- Local build and signing first; one GitHub build only when asked.
- Windows builds an unsigned per-user NSIS installer, Linux a deb, an rpm and an AppImage (`scripts/sign-linux.sh` signs the
  three with minisign and `scripts/verify-release.sh` checks them; see [signing.md](signing.md)), each on the owner's own machine
  ([build-windows.md](build-windows.md), [build-linux.md](build-linux.md)); Zig is exactly 0.17.1 and `scripts/env.sh` and
  `scripts/env.ps1` find it. No guide builds on GitHub, and no tag or release is made by them; the one GitHub build is the manual `linux-packages.yml` workflow for the Linux packages of `0.0.1-beta2`.
- The server is one image built from `packaging/docker/Dockerfile` in three stages (the page with Node, the binaries with
  Zig and Rust, a slim runtime with `tini`), pinned by tag and digest. It runs as 10001 with a read-only root file system,
  drops every capability and adds CHOWN, KILL, SETGID and SETUID. `packaging/compose/` and `packaging/helm/holzwave/` are the
  two supported ways to run it ([server.md](server.md)). No registry image is published: you build it. The image is built and
  tested on a Docker host; nothing about it was run on a cluster.
- `scripts/web-container-test.sh` (modes `caps`, `isolation`, `image`, `multiarch`, `acme`, `all`) proves the image in
  containers, and `scripts/web-deploy-check.mjs` judges the Compose file and the chart against the rules with a self-test that
  breaks one rule at a time. `scripts/test-names.sh` proves that the desktop's test list only grew while the host crate was
  cut out of the app ([test-plan.md](test-plan.md) Part 1).

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
| A17 | `kubectl` and `talosctl` are the clients, `az` and GAM7 wrappers | measured: `kube-rs` adds 34 crates, 5.9 MB and 35 s of build, `tonic` 12 crates and a protobuf toolchain; the tools run exec-credential plugins and every auth method already; no official Rust Talos client exists |
| A18 | Integration specs are compiled in; the published JSON is data (the profile file and the view model), with parity tests | a loadable argument template that synced would be remote command execution on every device; non-goal 3 stands |
| A19 | The tools' credential stores are referenced, never imported | the keychain cannot hold a kubeconfig (Windows refuses a blob over 2560 bytes) and a materialised file would break "secrets only in the keychain" |
| A20 | A block selects per command and a shell pane gets an overlay kubeconfig; the tools' own files change only through a confirmed verb | nothing on disk changes by looking around, and a pane in one cluster cannot move another |
| A21 | Approval of an integration verb is a ticket resolved again at run time, decided by the window only, with a production guard | what the person saw is what runs, nothing of it exists on `wsh`, and a mistake in a production context costs a second press |
| A22 | The environment of a child is the inherited one minus loader and interpreter variables and the app's own | the AI command runner's rule would drop the variables credential plugins need; a synced profile must not be able to hijack the next run |
| A23 | The integrations run on this computer only | the tools hold your credentials on this computer; a remote run would need a second trust decision and is not planned |
| A24 | GAM7 output is read by the `csv` crate, pinned to one version, with caps around it | the text is written by other people into a domain's directory; a hand-written splitter would be a parser of hostile input, and the crate is the established reader |
| A25 | Text from a tool is cleaned in the core and again in the page | the view model is a published contract that a model need not have passed through the core's cleaning; the second pass is defence in depth, and both use the same list of hidden characters |
| A26 | The commands and the state of the shell live in `holzwave-host`, which has no Tauri in its tree | the server must run the same commands as the desktop without a window system; a test name list proves no test was lost in the move |
| A27 | One macro table lists every command once and writes both the desktop's wrappers and the server's registry | a command cannot exist for one way only; a recorded surface of all commands and the protocol golden catch drift |
| A28 | One worker process per person, started by the supervisor through the spawn helper | a worker holds a person's panes, files and secrets; the operating system, not a check in our code, keeps two people apart |
| A29 | File capabilities on one small helper and user ids from a range, not user namespaces; no process runs as user id 0 | user namespaces are off or restricted on many clusters and kernels; four capabilities on one audited file are smaller than a root process, and the namespace needs only the baseline Pod Security level |
| A30 | The key of a person's saved passwords comes from the sign-in and lives in the worker's memory while a session is open | the server then holds no key at rest for it; an administrator can reset a person's secrets and can never read them |
| A31 | One replica on a local volume with SQLite, `synchronous=FULL` | the data is small and the credentials matter more than speed; a second writer would corrupt the store, so the chart has no replica value |
| A32 | The WebSocket sits behind a `Transport` trait; compression is off | the measured library saved 79 to 94 percent of the bytes and refused bombs but left 18 Autobahn cases unimplemented, which the adoption rule does not allow; one file changes if that changes |
| A33 | The single sign-on cookie is `SameSite=Lax`, single use and bound to the browser; every other cookie is `Strict` | the identity provider's redirect back is a cross-site navigation that a `Strict` cookie would not survive |
| A34 | The server never makes its own key: `keygen` writes it and the operator keeps a copy apart from the volume | a key made at first start would sit in the volume it protects, in an image layer or in a log |
| A35 | The image is built by the operator from the Dockerfile; no registry image is published | the project runs no registry, and the same Dockerfile builds for amd64 and arm64 |

## 15. Open questions

1. ~~Tauri 3 stability for Android, to be checked when phase 9 starts.~~ Answered on alpha.5: the app built and ran on three arm64
   emulator images (never on a phone), and the two Android bugs that were open upstream (an event emitted on the event-loop thread
   deadlocks the window thread, and reading a web view's address freezes it for ten seconds) were still open on 2026-11-06 and were not
   triggered by any run; the app never reads that address and calls its own plugin from worker threads. alpha.6 exists since
   2026-11-06 and was not taken: no Android file differs between the two, and it breaks window event handling that the desktop depends
   on. What the first build needed, each one a workaround and not a fix upstream: `tauri android init` with `--skip-targets-install`
   (without it the command line adds three more Rust targets); the four ABIs of the generated project cut to arm64-v8a in the
   project's own build plugin; the window restore code (it calls `maximize`, which Android does not have) and the `keyring` crate
   feature compiled out of the Android build; the Gradle flavour that the command line builds is called `universal` and holds the one
   ABI; the template's manifest hardened (no backup, its file provider and TV lines removed, a receiver of a library removed from the
   merged manifest); the plugin type is erased (`DynRuntime`) in alpha.5; and a key event of the Android web view with an empty `code`
   gets its code from its `key` in the page. Text tests of the Android project and the APK check keep the ABI, the manifest and the
   permission list in place.
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
7. The JSON shapes of real Talos output (nodes, services, machine status), the role member of `talosctl config info`, and
   whether `talosctl config context` writes the configuration file. **Open, see rows INT-T1 and INT-T2.** The Talos
   parsers are written from the tool's documents and the six Talos fixtures are written, not recorded, until a real
   talosconfig is used; an unknown shape reads as plain text, never an error.
8. How `az.cmd` on Windows treats the argument character set the integrations allow, and whether any name passes
   through a batch file unharmed. **Open, see rows INT-W2 and INT-W1.** What is built: an argument outside a strict
   character set is refused before a process exists (tested with injected Windows facts on the Mac); a real `az.cmd` has
   never been run.
9. The `PATH` of an app started from the Finder, and the cost of asking a login shell for its `PATH` on fish and nushell.
   **Answered in part.** A Finder-launched app on the research Mac (macOS 26) saw only `/usr/bin:/bin:/usr/sbin:/sbin`, so
   none of the four tools was on it; the login-shell probe found the directories of `kubectl` and `az` and took about 7 to
   12 ms with a synthetic home. fish is covered by a written capture and not a recording, and nushell is not asked (the
   fixed directories and the path field apply). **Open, see row INT-P1** for the built app.
10. Whether a fake API server for the real `kubectl` can run in the owner's test run. **Answered.** A local fake API server
    (Python, `socketserver`) lets the real `kubectl` 1.36.1 produce the recorded pods and namespaces; the Kubernetes parsers
    are tested against that recorded output and against a fake program. A real cluster is **open, see rows INT-K1 to
    INT-K4**.
11. Whether the chart runs on a real cluster at the baseline Pod Security level, with the init container, the single-file
    secret mounts and the group-readable key that kubelet makes. **Open, see rows SRV-K1 and SRV-K2.** The chart is linted,
    rendered and judged by the checker, and its configuration is judged by the server's own `check-config`; it was never
    applied.
12. Whether passkeys and the PRF extension behave on real platform authenticators and hardware keys in the four big browsers.
    **Open, see rows SRV-P1, SRV-P2 and SRV-B1.** The tests use virtual authenticators, and the passkey browser tests run in
    Chromium only.
13. Whether a real identity provider (Keycloak, Entra ID, Google and the like) behaves as the test provider does. **Open, see
    row SRV-I1.**
14. Whether a binary wire format would pay for itself. **Open.** JSON text frames are used because the page and the desktop
    already speak them; compression is off (section 16). A change needs a measurement on a real network first.
15. Workers that survive a server restart, so that a restart does not end the panes. **Open, not planned.** A restart ends
    every pane and every remote session, and the guide says so; a session therefore outlives every client but not the
    server. Whether a real workload behaves as the tests show across a restart was never tried (row SRV-S1).
16. User namespaces as a stronger boundary than user ids and file capabilities. **Open.** They are off or restricted on many
    clusters, so the design uses the four capabilities (A29).

The rows of Part 9 of [test-plan.md](test-plan.md) close the open parts of questions 7 to 10; an answer becomes a fixture and
a rule.

The rows of Part 10 of [test-plan.md](test-plan.md) close the open parts of questions 11 to 13; an answer becomes a test or a
sentence of the guide.

## 16. The web build

The same core, behind a browser. People sign in to one server, and each person's terminals run in a worker that is a user of
their own inside one container. Built and tested against a test identity provider, virtual authenticators, the container tests
and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider,
hardware authenticators or real networks. The rows that close those gaps are Part 10 of [test-plan.md](test-plan.md); the guide
for an operator is [server.md](server.md).

### The crates

`holzwave-core` stays free of Tauri and of any server type. `holzwave-host` sits on it and holds the commands, the state and the
host modules of the shell with no Tauri in its tree. The desktop shell (`holzwave-app`) and `holzwave-server` both sit on the
host crate, so the two ways to run holzWave answer the same commands from one table. `holzwave-spawn-helper` has no
holzWave dependency; the server takes only its request and answer types, so the client of the supervisor and the helper cannot
drift apart. The direction is core, then host, then the app or the server. The proof is the closure guard in
`core/tests/project_policy.rs` (the desktop app's closure holds no server-only library and the host crate's none of Tauri) and
the name list of `sh scripts/test-names.sh`, which showed that the move lost no test.

### The processes and the users

The container's first process is `tini`. Under it the supervisor (`holzwave-server serve`) runs as user 10001 with no
capability. It starts the spawn helper at assembly time. The helper is one file, owned by root with the group 10001 and mode
0750, with the file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p`; it keeps them permitted, raises them only
while it acts, and refuses to run at all with a reduced set. For each person the supervisor asks the helper for a worker: the
helper makes the person's home through a directory descriptor, and starts the worker as a user id from the range
(`workers.uid_min` to `workers.uid_max`, 20000 to 29999 by default, never below 1000), with no capability, `no_new_privs`,
four descriptors, seven environment variables and the limits `workers.nproc` and `workers.nofile`. The worker runs the core
from the command table; its panes are shells in process groups under the person's user id. A worker ends when no socket of
the person has been attached for `workers.idle_timeout` (a day by default), and every pane ends with it. No process of the
container has user id 0. The proof is `server/tests/isolation.rs`, forty tests in a container with exactly the four
capabilities (`sh scripts/web-container-test.sh isolation`), the process-table audit in `server/tests/support/proc.rs`, and
the image checks of `sh scripts/web-container-test.sh image`.

### The web wire

Section 6 describes the frames, the session event and the close codes. One socket carries a person's commands and events for
every tab of the page; the sockets of one person are at most eight (an app's device socket counts among them), they share a
worker, and the socket that last sent input
to a pane owns its size. A frame event that does not fit a slow socket's queue is dropped and the page asks for a full frame; any
other message that does not fit closes the socket with `1013`. The transport is a trait in one file. The measurement of
`permessage-deflate` saved 79 to 94 percent of the bytes and refused decompression bombs within the memory bound, but left
18 cases of the Autobahn suite unimplemented, which the rule for adoption does not allow, so the WebSocket never offers
the extension and `transport.compression` accepts only `off`. The proof is `server/tests/ws.rs`, `server/tests/wire.rs`,
`server/tests/parity.rs`, `server/tests/order.rs` and `server/tests/deflate.rs`.

### Sign-in and the session

Three methods can be switched on: passkeys (user verification required, discoverable credentials preferred, the PRF extension
passed through when the browser returns it), a password with a six-digit code of an authenticator app, and any number of
OpenID Connect providers (authorization code with PKCE, `state` and `nonce`, a strict check of the ID token, an account matched
by issuer and subject and never by e-mail, `allowed_domains` and `allowed_groups` as the only gate). A server with no account
prints a one-time `hwb_` token on standard output at every start; it makes exactly one first administrator and expires after
an hour. Administrators invite people with `hwi_` links that live seven days. A session is a 43-character token whose hash
alone is stored; it ends after `sessions.idle` without input or `sessions.absolute` in total, and a change of a credential
asks for a sign-in within `sessions.step_up`. Throttling, cookies, headers and the origin rules are in section 10. The proof is
`server/tests/auth.rs`, `server/tests/password_totp.rs`, `server/tests/passkey.rs`, `server/tests/webauthn_flow.rs`,
`server/tests/oidc.rs`, `server/tests/enrol.rs`, `server/tests/sessions.rs`, `server/tests/throttle_audit.rs` and, in WebKit
and Chromium against the real server, `npm --prefix app run test:web`.

### The secret store and its key

A person's saved passwords and tokens (the AI keys, the integration secrets, the sync secrets) live in `<home>/.holzwave/` in
one file, `secrets.store`, that only the person's worker opens. The file is sealed with XChaCha20-Poly1305 under a random
256-bit data key; the data key is wrapped once for each way to unlock (the password, a passphrase, the PRF output of a
passkey, a recovery code), so a way can be added or removed without touching the entries. A wrong key, a locked store and a
damaged byte all end in the same text, `secrets are locked`. The wrapping key reaches the worker when the person signs in and
leaves its memory when the last session of the person ends, by sign-out, idle time, absolute time, disabling or deleting, with
one audit line. A sign-in finds or makes the store. An administrator can reset a person's secrets, which removes the file, and
can never read them. The proof is `server/tests/secretstore.rs`, `server/tests/unlock.rs`, `server/tests/sealed.rs` and
`server/tests/canary.rs`, which follows canary secrets of two people through every file, log line, `Debug` text, shell
environment and process argument.

### The data volume

One volume, `/data` in the image. `server/server.sqlite3` is the store (accounts, credentials, sessions, invitations,
throttle counters; mode 0600, write-ahead log, `synchronous=FULL`). `server/audit/` holds one file a day. `homes/<user id>/`
is a person's home (mode 0700 under a 0711 base) with `.holzwave/` for the secret store, `uploads/` and a private `.tmp`. The
ACME account and the issued certificate are sealed under the server key. The server key file is not on this volume, and is
backed up apart. The proof is `server/tests/store.rs`, `server/tests/audit.rs` and the persistence checks of
`sh scripts/web-container-test.sh image` (a restart keeps the volume).

### The image and the two deployment examples

The image is one binary pair on a slim base, run as 10001 with a read-only root file system and `tini`; its health check is the
binary's own `healthcheck`, which asks a private Unix socket and opens no port. `packaging/compose/` has a Compose file with a
one-shot `keygen` service, and `packaging/helm/holzwave/` a chart with one Deployment (strategy `Recreate`, no replica value,
a `ReadWriteOnce` claim), an init container that prepares the homes base and a private run directory, and an Ingress for the
proxy mode only. The security context is fixed, not a value. The proof is the `image` and `multiarch` modes of
`scripts/web-container-test.sh`, `helm lint --strict` and `helm template` for the three value sets, and the checker
`scripts/web-deploy-check.mjs` with its self-test. The chart was never applied to a cluster.

### The device features

The apps (desktop and Android) use a server in two more ways, each behind a switch that an administrator turns on and the
operator can forbid (`device_access` in the configuration): the server as the target of the encrypted sync, and sessions that
live on the server. The guide is [server.md](server.md); the sync side is in [sync.md](sync.md). Built and tested against the
real server in one process over the loopback address, against a mock of the app in WebKit and Chromium, and in real browsers
against the real server, on the author's Mac; never run on a real phone or desktop against a real server over a real network,
with a real passkey or single sign-on approving a device, behind a real reverse proxy for the device socket, with two real
people, on Android, or across a server restart under a real workload.

- **Two kinds of device access.** `DeviceOpen` routes take no credential (the start and the poll of the device grant, RFC 8628)
  and `Device` routes take only a bearer token in the `Authorization` header (the sync routes, `/api/device/me` and the device
  socket). The person's side of the grant, the lists of devices and the switches are session routes that need a recent sign-in.
  A route has exactly one kind, so a cookie never authenticates a device route and a token never a session route.
- **The gate in the edge.** One table (`server/src/http/features.rs`) names the prefix of every route of a feature:
  `/api/device/` (open while either feature is on), `/sync/v1/` and `/ws/device`. A layer in front of every route answers a path
  under a closed prefix with the answer of a path that does not exist, before any credential is read, so a probe cannot tell
  a feature that is off from one that was never built, and no new route is reachable without its gate (the route oracle of the
  prohibitions test checks both states). While remote sessions are off, the commands of the page's own socket that list,
  make, open or end a session are answered at the socket's edge with `Remote sessions are off on this server.` and never reach
  the worker.
- **The sync target.** Four routes mirror the WebDAV operations the engine uses (`ensure`, `list`, `get`, `put` with the
  conditions of `If-None-Match: *` and `If-Match`); the condition, the bounds, the pruning of old backups and the bookkeeping
  are one immediate transaction of the store, the entity tag is the SHA-256 of the stored bytes, and the client is a second
  implementation of the engine's `Backend` trait (`core/src/sync/server.rs`), so the engine and the format are unchanged.
- **The device socket.** `GET /ws/device` carries text requests with an id and binary frames for the terminal bytes; the
  vocabulary, the tags and every bound are in `core/src/remote/proto.rs`, which the server and the app both use. The
  supervisor authenticates the socket, bounds it, counts it against the person's sockets and forwards frames between it and the
  worker; a terminal byte goes nowhere else (no log, no audit line, no database row). A revocation or an ended account closes it within a second (4401), and the token is looked at again every
  `limits.session_recheck` for what nobody announced.
- **The session host and the lease in the worker.** A session is a headless pane of the core in the person's worker: it draws
  no frame, and a tap on its engine thread hands every byte to each attachment, with an atomic snapshot of the screen and a
  bounded tail of the scrollback at the moment of the attach (the engine's own formatter, no hand-written serializer). The
  writer lease is a pure state machine in the worker (`server/src/worker/lease.rs`): one holder, freed after
  `remote.writer_timeout` of silence, moved only by `takeover`. The host is the one place that sees every attachment, so the
  rule holds across every device and the browser window.
- **The wire.** The supervisor and the worker speak a sixth frame kind, `Ses` (kind 6), for the sessions of the device
  sockets, and the protocol version is 3; the first frame carries the four `remote` limits of the configuration. The decoder is
  strict and bounded like every other kind.
- **The browser window.** A pane of the browser window that shows a session attaches to the session host inside the worker,
  through the same link a desktop pane runs on, with no socket and no token. All tabs of a person share it, so they are one
  writer, and the worker beats for it while a browser tab of the person is attached; the lease's own silence rule frees it
  after the last tab went. The page draws nothing new for it: the worker renders the pane with an engine of its own, beside the
  session's, whose cost was not measured.

The proof is `server/tests/device_grant.rs`, `server/tests/device_tokens.rs`, `server/tests/device_access.rs`,
`server/tests/sync_target.rs`, `server/tests/sync_client.rs`, `server/tests/remote_sessions.rs`, `server/tests/session_host.rs`,
`server/tests/session_local.rs`, `server/tests/session_web.rs`, `server/tests/remote_client.rs`, `server/tests/remote_host.rs`,
`server/tests/remote_budget.rs`, `core/tests/remote_snapshot.rs`, `core/tests/remote_hub.rs`, `core/tests/remote_pane.rs` and,
in WebKit and Chromium against the real server, `app/tests/web/devices.test.mjs` and `app/tests/web/sessions.test.mjs`.

### The tests that prove the build

Server tests run on the Mac and in a Linux container; the isolation, capability and image tests need Linux and run in a
container only. The suites that cross the whole server are `server/tests/prohibitions.rs` (every route, header and
served file against an independent list), `server/tests/canary_sweep.rs` and `server/tests/audit_once.rs`. The page is tested
against the product's own server in WebKit and Chromium by `npm --prefix app run test:web`. The commands are listed in
[test-plan.md](test-plan.md) Part 1; the rows that need a real cluster, provider, authenticator, certificate authority, proxy,
browser or person are Part 10.

See also: [design.md](design.md) for colour, type, components and the window direction.
