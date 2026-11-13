# Sync

Status: **built** (`core/src/sync`, `host/src/sync.rs`, `app/src/sync.ts`): format, WebDAV transport, merge engine,
account, scheduled runs, popover. Tested against an in-memory WebDAV server and, with `scripts/nextcloud-live-test.sh`, against
a Nextcloud in Docker; two real computers, your own server and its app-password policy stay the owner's check (row S1 of
`docs/test-plan.md`).

The second transport, a holzWave server ([The holzWave server as the target](#the-holzwave-server-as-the-target)), is built
too (`core/src/sync/server.rs`, `server/src/sync_target.rs`). It is tested with the real sync engine and the real client against
the real server, in one process over the loopback address (`server/tests/sync_target.rs`, `server/tests/sync_client.rs`). It was
never run against a server on a real network, behind a real reverse proxy, between two real computers or phones, or on Android.

## What is synced

`layout.json`, `settings.json`, `connections.json`, `integrations.json`, `themes/<name>.json`, `widgets/<name>.json` and
`widget_notes.json`, plus the connection secrets (passwords and key passphrases from the OS keychain) and the secret
variables of the integration profiles (small values, at most 1 KiB each, from the OS keychain) as one virtual file,
`secrets.json`, which is never written to disk. Machine-local state (`session.json`, `local_network.json`, `known_hosts`) is
not synced. The two pane files (`pane_connections.json` and `pane_blocks.json`) are machine-local too, but their shared part travels inside the layout:

`layout.json` is synced as a **layout unit**: the layout, which panes run which saved connection, and the blocks whose
source is a saved connection (a remote file browser or preview) or the monitor of this computer. A local folder, a local
preview and a chat are tied to the computer they were made on and are never shared. No secret is part of the unit. The
unit is one file on the server like any other (`files/layout.json.hwx`); the format of the blob does not change.

A theme or widget file is synced under its file name without `.json`: letters, digits, `-` and `_`, at most 112 characters
for a theme and 111 for a widget, because the file on the server is `themes.<name>.json.hwx` (or `widgets.…`) and one name
on the server is at most 128 characters. A file with a longer name is not synced and the sync says so with the limit and the
actual length (the popover shows the first problem of a run, and it stays until the file is renamed); every other file
syncs. A file whose name holds another character (a space, a dot) is not synced either, and says nothing.

`integrations.json` holds the integration profiles: names and flags, never a credential or a path. Its secret variables are
listed there by name, and the values travel in `secrets.json` under the account `int:<profile id>:<NAME>`. A device that
receives `secrets.json` accepts exactly two account shapes, `ssh:<id>` for a saved connection and `int:<profile id>:<NAME>`
for a secret variable (the profile id and the variable name must be valid as the profile file defines them, the value is
1 to 1024 bytes without a NUL, and a payload holds at most 512 of them). One entry that does not fit refuses the whole
payload and nothing of it is applied, so a payload from another computer cannot set any other keychain entry. The accounts a
run sends are the ones the profile file declares, never a listing of the keychain. A downloaded `integrations.json` waits in
`sync-incoming/` like the other files the running app owns and is read at the next start by the same whole-file validation
as a file you wrote yourself: a file that breaks any rule (for example one that sets a variable that changes how a program
loads code) is kept as `integrations.json.bad` and holzWave starts without profiles. A device without this version ignores
the file, because its list of names does not have it.

Not synced: the machine-local integration files (`integration_local.json` with the paths of the tools on this
computer, `pane_integrations.json` and `pane_launches.json`), the private files holzWave writes for a running pane (the
overlay configuration under `run/`), and the tools' own credential files and configurations (the kubeconfig, the
talosconfig, the Azure and GAM directories), which holzWave refers to and never copies. Also not synced: `history.db`
(see "The history stays local" below), `ai.json` and `chats/` (the AI providers and the chats), and the keychain entries of the AI providers (a provider key stays on the computer where
you typed it, so enter it again on another one). The three AI settings (`ai_local_only`, `ai_local_context` and
`ai_persist_context`) are part of `settings.json` and do travel with it.

## Decisions

- **Transport:** one of two, and exactly one at a time. WebDAV (Nextcloud and others): the endpoint folder, a user and an
  app password. Or a holzWave server that the app is signed in to as a device ([server.md](server.md#device-sign-in-and-devices)):
  its address and the device token in the OS keychain. Both are `https` only (loopback may use `http` for tests), follow no
  redirect (a credential never goes to another host) and bound every answer. The passphrase, the encrypted files, the
  version stamps and the conflict rule below are the same for both; the server is a second place to keep the same files,
  not a second format.
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
  (`If-Match` / `If-None-Match: *`); a refused write is looked at again, up to three times. The ETag is only a hint, see
  "Same-second writes".
- **Same-second writes:** Nextcloud's ETag changes once per second, so two writes of one name inside a second can share it.
  Every upload therefore carries `OC-Checksum: sha256:<hex>` of the sealed blob (never of the plaintext, which the server
  never learns anything about), the listing asks for `oc:checksums`, and the identity of a server version is the SHA-256 of
  its blob, which the local state records: a reported checksum that differs from the recorded one makes the device read the
  blob, and the blob decides, so a server that reports a wrong checksum costs one read and never a false change. Before a
  device overwrites an existing server file (an edit, the winning side of a conflict, a tombstone) it reads the current
  one and compares it with the one it last saw; a different one is a change on the server and takes the conflict path
  (the higher stamp wins, the loser is kept, an edit beats a delete). The cost is one read per upload and none for a quiet
  run. The limit on a server that reports no checksum (plain WebDAV): a change written in the same second as the version a
  device last saw is noticed when that device next changes the file, not at its next run; nothing is lost, the other
  version is kept. The one race that is left is a third write between a device's read and its write inside that same
  second. A write without the checksum header (the client never does that) leaves a Nextcloud file with no reported
  checksum, so the ETag and the read before a write are what protect it then. There is no rollback protection: a server
  that replays an old valid blob is the limit the sync has for any file.
- **Conflicts:** when both sides changed, the higher stamp wins and the loser is kept: a replaced server version in
  `backups/` on the server (named by the file name, the version's counter and the writer's id; a name too long for that
  is shortened to its first 64 characters, a hash and `.hwx`), a replaced local version in `sync-conflicts/` next to the
  files (never the secrets).
  A download is staged first. A local file the person changed after the download was staged is copied to
  `sync-conflicts/` first (the file name with `/` as `.`, then the time), and the run after that lists it as a conflict.
  The copy is the whole file; nothing is merged. The same rule covers both ways a staged file reaches its place (below).
- **What is applied at once:** after every run the app moves into place what it does not own: the themes, the widgets
  and the texts of the note widgets (`themes/*.json`, `widgets/*.json`, `widget_notes.json`). A theme is announced to
  every window, so the theme list shows it and a selected theme that was replaced is drawn again in the panes; a widget
  or a note shows on the sidebar at its next refresh (every 10 seconds while the sidebar is open). A note save and the
  replacement of `widget_notes.json` never run together, so a note typed during a download is not lost to a
  half-replaced file. Connection secrets also apply at once.
- **What waits for a start:** `layout.json`, `settings.json`, `connections.json` and `integrations.json` stay in `sync-incoming/` and are
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
  `themes/` and `widgets/` only: `layout.json`, `settings.json`, `connections.json`, `integrations.json`, `widget_notes.json` and the secrets
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
  a corrupt or hostile file never wipes the keychain. Until that start the secrets a run carries follow the staged list
  and not the live one, so a profile another computer removed does not have its secret uploaded again in between.
- **The history stays local:** `history.db` is a database, not a file per item, so it cannot be synced entry by entry
  without a tombstone for every command, and a command you forgot must not come back on another computer. That is a
  decision, not a gap, and it can be reopened: syncing the history would be opt-in and would need those per-entry
  deletions first.
- **Adding a device:** with WebDAV, enter the endpoint, user, app password and the same passphrase; with a holzWave server,
  sign the app in to the server and enter the same passphrase. There is no QR code.
- **Local state:** `sync_state.json` (device id; per file the counter, the ETag, the SHA-256 of the content and the SHA-256
  of the blob that the last run left behind), `sync.json` (endpoint, user and `transport`: `webdav`, which is also what a
  file without the member means, or `server`, with the address of the server as the endpoint and no user), `sync-incoming/` (the downloads of the files the running app owns, which wait for the next start, with a
  record of the live file's hash at the time each was staged) and `sync-kept.json` (the names kept when a download replaced an edit,
  removed by the next run). The app password and the passphrase are in the OS keychain.

## The holzWave server as the target

A holzWave server ([server.md](server.md)) can keep the encrypted files of a person instead of a WebDAV folder. An operator turns
this on: the switch `Sync target` is off on every server until an administrator turns it on
([server.md](server.md#the-two-switches)); while it is off the popover says `Sync is off on this server.` The sync popover of
the desktop and Android apps offers `holzWave server` next to `WebDAV` ([design.md](design.md) section 6.9); it needs the app
to be signed in to the server as a device, once, in the person's browser. The window in a browser offers WebDAV only.

- **What the server sees.** What a WebDAV server sees: the names of the files, their sizes, the version stamp and the random
  id of the writing device in each header, and the time of each write. It checks the structure of a file (the header of
  the format, without a key) and its size, and never opens it. It holds no key, never learns the passphrase and writes no
  path, name or byte of a file to its log or its audit log. The files are rows of the server's database and are deleted with
  the person's account.
- **Three shapes of a path.** `meta.json`, `files/<name>` and `backups/<name>`, where the name passes the same check as on
  WebDAV; anything else is refused before the store is asked. `meta.json` is written once and holds at most 4 KiB; the file
  the engine writes is 269 bytes.
- **Writes.** The same conditional writes as on WebDAV (`If-None-Match: *` for a new file, `If-Match` for a replacement), so a
  write that lost a race takes the conflict path above. The server's entity tag, and the checksum its listing reports, is the
  SHA-256 of the stored bytes, so two different writes never share one and the same-second limit of Nextcloud does not
  arise.
- **Bounds per person**, set by the operator ([config.md](config.md#the-server-configuration)): `sync.quota_mib` (64 MiB by
  default), `sync.max_files` (512, `meta.json` and the backups included), `sync.backups_keep` (20: after each backup is
  written, the oldest beyond this number are deleted on the server) and `sync.max_blob_bytes` (8 MiB, the largest file of
  the format; the operator can only make it smaller). A write past the space or the file count is refused with the code
  `quota` and stores nothing, and a file over the size with `too_large`; the popover shows the sync's own sentence for each
  (`The file is too large for the server.`, `The space on the server is used up.`).
- **Changing the transport copies nothing.** No file moves from WebDAV to the server or back, and no file is deleted on
  either. What the engine recorded about the old target is set aside (`sync_state.json.previous`), so the first run treats
  the new target as new and never reads an empty server as the deletion of the files of this computer; a first run that
  fails puts everything back. After the first successful run against the server the WebDAV app password is removed from
  the keychain. A sync that was set up with one server refuses a run against another (`The sync was set up with another
  server. Turn it on again for this one.`).
- **Signing out.** `Sign out` asks the server to end the device's sign-in, removes the token from the keychain and keeps the
  address, so the next `Sign in…` goes straight to the code. A device whose token was revoked, or ran out after 90 days
  without use (the default of the server), is treated the same way: the sync shows `signed_out` and makes no run until the person signs in again.
- **Never synced.** The link to the server (`server_link.json`: the address, the device id and the device name) and the
  record of which pane shows which server session (`remote_panes.json`) are files of this computer and are not on the sync's
  list. The token is only in the OS keychain, under the account `server:token`, and the receiver of `secrets.json` refuses
  that account (it accepts `ssh:<id>` and `int:<profile id>:<NAME>` only), so no other computer can set it.
- **The limit the format always had.** A server can serve an older valid file instead of the newest one; nothing in the
  format detects that (no rollback protection), on the server exactly as on WebDAV.

Tested with the real engine on two devices of one person against the real server in one process: the same files arrive with
one passphrase, the newer stamp wins and the older version is kept in `backups/`, a lost race is settled, another person can
read, list or overwrite nothing, a wrong passphrase writes nothing, and no file the server writes holds a setting, the
passphrase or a key (`server/tests/sync_target.rs`, `server/tests/sync_client.rs`). Not run: a server on a real network or
behind a real reverse proxy, two real computers or phones, Android, and a person near the bounds over a long time.

## Not yet

- Deleting an integration profile on one computer does not delete its secret variables on another: deletions are synced
  only for themes and widgets, and the value of a secret variable stays in the other computer's keychain until you remove
  it there.
- A downloaded `layout.json`, `settings.json`, `connections.json` or `integrations.json` is still staged in `sync-incoming/` and moved into
  place at the next start (the running app owns these files); the popover says so. A local edit made in between is kept
  as a copy in `sync-conflicts/`. Applying them while the app runs would need the settings, the layout and the
  connection list to be reloaded from their files, which nothing asks for yet.
- A run happens 10 s after the start and every five minutes, and with `Sync now`; not yet on exit.
- One target at a time: a device syncs with WebDAV or with a holzWave server, never with both, and never with two servers.
- Moving the files from one target to the other: changing the transport copies nothing (above); the first run against the
  new target uploads what this computer holds, and the old target keeps its files until you delete them there.
- The window in a browser cannot use its own server as the target; the choice is in the desktop and Android apps.
