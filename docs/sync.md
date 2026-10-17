# Sync

Status: **built** (`core/src/sync`, `app/src-tauri/src/sync.rs`, `app/src/sync.ts`): format, WebDAV transport, merge engine,
account, scheduled runs, popover. Tested against an in-memory WebDAV server, not yet against a real Nextcloud.

## What is synced

`layout.json`, `settings.json`, `connections.json`, `themes/<name>.json`, `widgets/<name>.json` and `widget_notes.json`,
plus the connection secrets (passwords and key passphrases from the OS keychain) as one virtual file, `secrets.json`, which
is never written to disk. Machine-local state (`session.json`, `known_hosts`) is not synced. The two pane files
(`pane_connections.json` and `pane_blocks.json`) are machine-local too, but their shared part travels inside the layout:

`layout.json` is synced as a **layout unit**: the layout, which panes run which saved connection, and the blocks whose
source is a saved connection (a remote file browser or preview) or the monitor of this computer. A local folder, a local
preview and a chat are tied to the computer they were made on and are never shared. No secret is part of the unit. The
unit is one file on the server like any other (`files/layout.json.hwx`); the format of the blob does not change.

Also not synced: `history.db` (deletions are not synced, so a command you forgot would come back), `ai.json` and `chats/`
(the AI providers and the chats), and the keychain entries of the AI providers (a provider key stays on the computer where
you typed it, so enter it again on another one). The three AI settings (`ai_local_only`, `ai_local_context` and
`ai_persist_context`) are part of `settings.json` and do travel with it.

## Decisions

- **Transport:** WebDAV (Nextcloud and others): the endpoint folder, a user and an app password. `https` only (loopback
  may use `http` for tests), no redirects (the password never goes to another host), every answer bounded. No server
  component of our own.
- **Encryption:** end to end. Key: Argon2id (64 MiB, 3 passes, 1 lane by default; the parameters and a random 16 byte
  salt are in the plain `meta.json` of the folder, and bounded on reading) from the user's passphrase. Cipher:
  XChaCha20-Poly1305 with a random 24 byte nonce per write. Both from the audited RustCrypto crates. A lost passphrase
  cannot be recovered; `meta.json` carries a sealed check text so a wrong passphrase is told at once.
- **Granularity:** one blob per file: `files/<name>.hwx` (`themes/dark.json` is `files/themes.dark.json.hwx`).
- **Blob format (version 1):** `"HWX" 0x01 | counter u64 | time i64 | device [16] | nonce [24] | ciphertext+tag`. The
  header and the file name are authenticated as associated data: a blob cannot be moved to another name or get a
  different version stamp. The server sees names, sizes, the stamp and the writer's random device id, never a content.
- **Versions:** a counter per file (every write is the highest seen plus one); the writer's clock only breaks a tie, so
  a device with a wrong clock cannot overwrite newer work. The server's ETag detects a concurrent write
  (`If-Match` / `If-None-Match: *`); a refused write is looked at again, up to three times.
- **Conflicts:** when both sides changed, the higher stamp wins and the loser is kept: a replaced server version in
  `backups/` on the server, a replaced local version in `sync-conflicts/` next to the files (never the secrets).
  A download is staged first. A local file the person changed after the download was staged is copied to
  `sync-conflicts/` first (the file name with `/` as `.`, then the time), and the run after that lists it as a conflict.
  The copy is the whole file; nothing is merged. The same rule covers both ways a staged file reaches its place (below).
- **What is applied at once:** after every run the app moves into place what it does not own: the themes, the widgets
  and the texts of the note widgets (`themes/*.json`, `widgets/*.json`, `widget_notes.json`). A theme is announced to
  every window, so the theme list shows it and a selected theme that was replaced is drawn again in the panes; a widget
  or a note shows on the sidebar at its next refresh (every 10 seconds while the sidebar is open). A note save and the
  replacement of `widget_notes.json` never run together, so a note typed during a download is not lost to a
  half-replaced file. Connection secrets also apply at once.
- **What waits for a start:** `layout.json`, `settings.json` and `connections.json` stay in `sync-incoming/` and are
  moved into place when the app starts again, because the running app rewrites them from memory and would overwrite a
  file replaced under it. The popover asks for a restart only while such a download waits (or a staged file could not
  be applied).
- **Adopting a layout:** pane ids are small per-device counters, so two devices use the same numbers for different
  panes. A layout taken over without the assignments that belong to it would meet the other device's own pane files:
  a saved host or a file block could land on another pane, and a connected pane of the other device would come up as a
  plain shell. So an adoption replaces this device's `pane_connections.json` and `pane_blocks.json` with the ones of the
  unit, as one step, with the layout (the last writer wins, as for every file). The files it replaces are copied to
  `sync-conflicts/` first (the file name, then the time); when a copy cannot be written nothing is applied and the unit
  stays staged. The working directories kept in `session.json` for the old panes are forgotten (the window frame is
  kept). A layout uploaded by an older build, a plain layout without assignments, is still accepted and clears the
  assignments. A downloaded unit is checked before it is staged and again before it is applied: a layout that is not
  an object, a pane that is not a number, a connection id that is not a plain name, a member of another version or
  more than 4096 entries refuse it. A device that never adopts a layout keeps its assignments as they are; building the
  unit changes none of its files.
- **Adding a device:** enter the endpoint, user, app password and the same passphrase. A QR code comes with mobile.
- **Local state:** `sync_state.json` (device id; per file the counter, ETag and SHA-256 of what the last run left
  behind), `sync.json` (endpoint, user, on/off), `sync-incoming/` (the downloads of the files the running app owns, which wait for the next start, with a
  record of the live file's hash at the time each was staged) and `sync-kept.json` (the names kept when a download replaced an edit,
  removed by the next run). The app password and the passphrase are in the OS keychain.

## Not yet

- Deletions are not synced; a deleted file comes back on the next run.
- A downloaded `layout.json`, `settings.json` or `connections.json` is still staged in `sync-incoming/` and moved into
  place at the next start (the running app owns these files); the popover says so. A local edit made in between is kept
  as a copy in `sync-conflicts/`. Applying them while the app runs would need the settings, the layout and the
  connection list to be reloaded from their files, which nothing asks for yet.
- A run happens 10 s after the start and every five minutes, and with `Sync now`; not yet on exit.
