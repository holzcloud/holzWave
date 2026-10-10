# Sync

Status: **open.** This page lists what the format has to satisfy so that it is decided *before* any sync code
exists. It is the first task of phase 3b.

## What is synced

Settings, themes, widgets, connection profiles and layouts. Secrets only inside the encrypted payload.

## Requirements

- **Transport:** WebDAV, including Nextcloud. An endpoint URL plus an app password. No server component of our own.
- **Confidentiality:** end-to-end encrypted with a user passphrase; the server sees only ciphertext and file sizes.
  Candidates: `age`, or XChaCha20-Poly1305 with a passphrase-derived key.
- **Conflicts:** a version stamp per file; last writer wins; the replaced version is kept as a backup.
- **Platforms:** the same format and the same Rust implementation on desktop, iOS and Android.
- **Evolvability:** a format version in every file, and a defined migration path, because changing the format later
  means re-encrypting every user's data.

## To decide

1. One encrypted blob per file, or per record?
2. Key derivation parameters, and how a lost passphrase is handled (it cannot be recovered).
3. How a device is added: re-enter the passphrase, or a QR code from an existing device.
4. How the stamp is made (wall clock, counter, or both) given devices with wrong clocks.
