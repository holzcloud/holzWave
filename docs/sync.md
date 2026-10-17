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

Also not synced: `history.db` (see "The history stays local" below), `ai.json` and `chats/`
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
- **Deletions:** deleting a theme or a widget on one computer deletes it on the others, and it does not come back. The
  deleting computer uploads a *tombstone*: the sealed payload `NUL` followed by `holzwave:deleted`, inside the same
  authenticated encryption as every file, so the blob format stays version 1. It is not valid JSON, so an older build that
  stages it as a theme or widget file sees an invalid file and leaves it out. A computer that finds a tombstone over a
  file it has not changed since the last run copies the file to `sync-conflicts/` and removes it (the staged download of
  it goes too); a computer that never had the file records the tombstone and creates nothing. Tombstones exist for
  `themes/` and `widgets/` only: `layout.json`, `settings.json`, `connections.json`, `widget_notes.json` and the secrets
  are never deleted by the sync, and a tombstone for one of them is refused as an error of that name. An edit beats a
  delete on either side: a file changed on one computer while another deleted it is kept and comes back to the other
  (listed as a conflict), and a file created again after a deletion supersedes the tombstone. The safety rules:
  - a file counts as deleted only when it is not found; a read error, a file that is not a regular file and an unreadable
    folder never count, and a folder that is missing is an error (`the folder themes is missing; no deletion was sent`)
    that deletes nothing;
  - a run sends at most 5 tombstones and removes at most 20 files. More than 5 deletions at once are not sent at all and
    are reported as an error naming the folder, so a mass deletion (or a wiped folder) stops whole; a person who deletes
    more than five files at once does it in batches, with a run between them. Removals past 20 wait for the next run
    and are reported the same way;
  - the file is copied to `sync-conflicts/` before it is removed, and when the copy cannot be written it is not removed.

  Tombstones are not pruned, and a replay of an old valid blob is the same limit the sync has for any file (there is no
  rollback protection). The popover says how many files another computer removed in the last run, and the theme list
  follows at once.
- **Connections and their secrets:** `connections.json` is one file, so a connection deleted on one computer travels as an
  edit of it and is gone from the others at their next start. When the new list is applied and no longer holds a profile
  that the replaced one held, the keychain entry (`ssh:<id>`) of exactly that profile is deleted. Nothing is deleted when
  the new file does not read as a connection list, when none of its profiles is usable, or for a profile it still names:
  a corrupt or hostile file never wipes the keychain.
- **The history stays local:** `history.db` is a database, not a file per item, so it cannot be synced entry by entry
  without a tombstone for every command, and a command you forgot must not come back on another computer. That is a
  decision, not a gap, and it can be reopened: syncing the history would be opt-in and would need those per-entry
  deletions first.
- **Adding a device:** enter the endpoint, user, app password and the same passphrase. A QR code comes with mobile.
- **Local state:** `sync_state.json` (device id; per file the counter, ETag and SHA-256 of what the last run left
  behind), `sync.json` (endpoint, user, on/off), `sync-incoming/` (the downloads of the files the running app owns, which wait for the next start, with a
  record of the live file's hash at the time each was staged) and `sync-kept.json` (the names kept when a download replaced an edit,
  removed by the next run). The app password and the passphrase are in the OS keychain.

## Not yet

- A downloaded `layout.json`, `settings.json` or `connections.json` is still staged in `sync-incoming/` and moved into
  place at the next start (the running app owns these files); the popover says so. A local edit made in between is kept
  as a copy in `sync-conflicts/`. Applying them while the app runs would need the settings, the layout and the
  connection list to be reloaded from their files, which nothing asks for yet.
- A run happens 10 s after the start and every five minutes, and with `Sync now`; not yet on exit.
