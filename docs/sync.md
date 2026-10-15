# Sync

Status: **core built** (`core/src/sync`): format, WebDAV transport, merge engine, tested against an in-memory WebDAV
server. The app layer (settings, background run, dialog) follows.

## What is synced

`layout.json`, `settings.json`, `connections.json` and `themes/<name>.json`, plus the connection secrets (passwords and
key passphrases from the OS keychain) as one virtual file, `secrets.json`, which is never written to disk. Machine-local
state (`session.json`, `known_hosts`, `pane_*.json`) is not synced.

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
- **Adding a device:** enter the endpoint, user, app password and the same passphrase. A QR code comes with mobile.
- **Local state:** `sync_state.json` (device id; per file the counter, ETag and SHA-256 of what the last run left
  behind), `sync.json` (endpoint, user, on/off). The app password and the passphrase are in the OS keychain.

## Not yet

- Deletions are not synced; a deleted file comes back on the next run.
- A downloaded `settings.json`, `layout.json`, theme or connection list is read at the next start.
