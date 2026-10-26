//! The worker's encrypted secret store: one file, opened with a key it is sent and never written in clear.
//!
//! [`EncryptedFileStore`] implements the core's [`SecretStore`] over one file. A random 256-bit data key encrypts every
//! entry with XChaCha20-Poly1305 and a fresh 24-byte nonce for every write. The data key is wrapped once per unlock method
//! (a slot), each under that method's own key-encryption key ([`Kek`]); a slot can be added, replaced or removed without
//! touching the encrypted entries.
//!
//! File format, version 1 (all numbers big endian):
//!
//! ```text
//! "HWSS" | version u16 | slot count u8 |
//!   per slot: kind u8 | id [u8; 16] | nonce [u8; 24] | wrapped data key [u8; 32] + tag [u8; 16] |
//! body nonce [u8; 24] | body ciphertext (the JSON object of entries) + tag [u8; 16]
//! ```
//!
//! The body is sealed with the associated data `"HWSS"` and the version; a slot is sealed with `"HWSS"`, the version, its
//! kind and its id. Someone who can write the file can destroy it and nothing more: every change to it ends in the one
//! error [`LOCKED`] (a wrong key looks exactly like a damaged slot) or in a format error, and never in plaintext.
//!
//! Every write is a read-modify-write under an exclusive `flock` on a sidecar file next to the store: the new file is
//! written to a temporary file in the same directory (mode 0600, exclusive creation), synced, renamed over the old one, and
//! the directory is synced. A temporary file left by a crash is removed at the next open.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use holzwave_core::secrets::{MAX_LEN, SecretStore};
use holzwave_core::{Error, Result};
use rustix::fs::{FlockOperation, OFlags};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::unlock::{KEY_BYTES, Kek, UnlockError, random};

/// The text of the error of every locked operation.
pub const LOCKED: &str = "secrets are locked";

/// The first bytes of the file.
pub const MAGIC: &[u8; 4] = b"HWSS";

/// The format version this code writes and reads.
pub const VERSION: u16 = 1;

/// The most slots a store holds.
pub const MAX_SLOTS: usize = 16;

/// The largest store file in bytes.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// The most entries a store holds.
pub const MAX_ENTRIES: usize = 4096;

const ID_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const WRAPPED_BYTES: usize = KEY_BYTES + TAG_BYTES;
/// Kind, id, nonce, wrapped data key with its tag.
const SLOT_BYTES: usize = 1 + ID_BYTES + NONCE_BYTES + WRAPPED_BYTES;
/// Magic, version, slot count.
const FIXED_HEADER_BYTES: usize = 4 + 2 + 1;
/// The shortest body: the nonce and the tag around `{}`.
const MIN_BODY_BYTES: usize = NONCE_BYTES + 2 + TAG_BYTES;

/// What the `Debug` text of the data key prints.
const DEK_DEBUG: &str = "Dek(..)";

/// The way to unlock a store. Each kind owns its own slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotKind {
    /// The account password.
    Password = 1,
    /// A passphrase chosen for the secrets.
    Passphrase = 2,
    /// A passkey's pseudo-random function result.
    Prf = 3,
    /// The recovery code.
    Recovery = 4,
}

impl SlotKind {
    fn from_byte(byte: u8) -> Option<SlotKind> {
        match byte {
            1 => Some(SlotKind::Password),
            2 => Some(SlotKind::Passphrase),
            3 => Some(SlotKind::Prf),
            4 => Some(SlotKind::Recovery),
            _ => None,
        }
    }
}

/// Why a store operation failed. None of the texts holds a key, a secret, an account name or a byte of the file.
#[derive(Debug)]
pub enum StoreError {
    /// The store is locked, the key is wrong, or the file was changed. These cases are not told apart.
    Locked,
    /// The file is not a store of this version.
    Format(&'static str),
    /// A limit of the store would be exceeded.
    Limit(&'static str),
    /// A slot operation was refused.
    Slot(&'static str),
    /// A store exists at that path already.
    Exists,
    /// The account or the secret does not fit the rules of the trait.
    Invalid(&'static str),
    /// The operating system gave no random bytes.
    Random,
    /// The file system failed.
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Locked => f.write_str(LOCKED),
            StoreError::Format(why) => write!(f, "the secret store file is not valid: {why}"),
            StoreError::Limit(why) => write!(f, "the secret store would be too large: {why}"),
            StoreError::Slot(why) => write!(f, "the secret store slot was refused: {why}"),
            StoreError::Exists => f.write_str("a secret store exists at that path already"),
            StoreError::Invalid(why) => f.write_str(why),
            StoreError::Random => f.write_str("the operating system gave no random bytes"),
            StoreError::Io(e) => write!(f, "the secret store file could not be used: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<UnlockError> for StoreError {
    fn from(_: UnlockError) -> Self {
        StoreError::Random
    }
}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Error {
        match e {
            StoreError::Invalid(why) => Error::Unsupported(why.into()),
            StoreError::Locked => Error::Io(io::Error::new(io::ErrorKind::PermissionDenied, StoreError::Locked)),
            StoreError::Io(e) => Error::Io(e),
            other => Error::Io(io::Error::other(other)),
        }
    }
}

/// Whether `error` came from a locked store.
pub fn is_locked(error: &Error) -> bool {
    match error {
        Error::Io(e) => e.get_ref().and_then(|inner| inner.downcast_ref::<StoreError>()).is_some_and(|s| matches!(s, StoreError::Locked)),
        _ => false,
    }
}

/// The data key: it encrypts the entries. Wiped when dropped, never printed, not `Clone`.
pub struct Dek(Zeroizing<[u8; KEY_BYTES]>);

impl Dek {
    /// A new random data key.
    pub fn random() -> std::result::Result<Dek, StoreError> {
        Ok(Dek(Zeroizing::new(random::<KEY_BYTES>()?)))
    }

    /// A data key from 32 bytes. The caller wipes its own copy.
    pub fn from_bytes(bytes: &[u8; KEY_BYTES]) -> Dek {
        Dek(Zeroizing::new(*bytes))
    }
}

impl fmt::Debug for Dek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DEK_DEBUG)
    }
}

/// One entry of the body. The text is wiped when the entry is dropped.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Entry(String);

impl Drop for Entry {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

type Entries = BTreeMap<String, Entry>;

/// One slot as it is in the file.
#[derive(Clone)]
struct Slot {
    kind: SlotKind,
    id: [u8; ID_BYTES],
    nonce: [u8; NONCE_BYTES],
    wrapped: [u8; WRAPPED_BYTES],
}

/// A parsed file: the slots and the body (nonce and ciphertext) exactly as they were.
struct Parsed {
    slots: Vec<Slot>,
    body: Vec<u8>,
}

fn parse(bytes: &[u8]) -> std::result::Result<Parsed, StoreError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(StoreError::Limit("the file is larger than 4 MiB"));
    }
    if bytes.len() < FIXED_HEADER_BYTES || &bytes[..4] != MAGIC {
        return Err(StoreError::Format("it does not start with the store marker"));
    }
    if u16::from_be_bytes([bytes[4], bytes[5]]) != VERSION {
        return Err(StoreError::Format("the version is not supported"));
    }
    let count = usize::from(bytes[6]);
    if count == 0 || count > MAX_SLOTS {
        return Err(StoreError::Format("the slot count is out of range"));
    }
    let slots_end = FIXED_HEADER_BYTES + count * SLOT_BYTES;
    if bytes.len() < slots_end + MIN_BODY_BYTES {
        return Err(StoreError::Format("the file is too short"));
    }
    let mut slots = Vec::with_capacity(count);
    for raw in bytes[FIXED_HEADER_BYTES..slots_end].as_chunks::<SLOT_BYTES>().0 {
        let kind = SlotKind::from_byte(raw[0]).ok_or(StoreError::Format("a slot has an unknown kind"))?;
        let mut id = [0u8; ID_BYTES];
        id.copy_from_slice(&raw[1..1 + ID_BYTES]);
        let mut nonce = [0u8; NONCE_BYTES];
        nonce.copy_from_slice(&raw[1 + ID_BYTES..1 + ID_BYTES + NONCE_BYTES]);
        let mut wrapped = [0u8; WRAPPED_BYTES];
        wrapped.copy_from_slice(&raw[1 + ID_BYTES + NONCE_BYTES..]);
        slots.push(Slot { kind, id, nonce, wrapped });
    }
    Ok(Parsed { slots, body: bytes[slots_end..].to_vec() })
}

fn serialize(slots: &[Slot], body: &[u8]) -> std::result::Result<Vec<u8>, StoreError> {
    let mut out = Vec::with_capacity(FIXED_HEADER_BYTES + slots.len() * SLOT_BYTES + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_be_bytes());
    out.push(u8::try_from(slots.len()).map_err(|_| StoreError::Slot("too many slots"))?);
    for slot in slots {
        out.push(slot.kind as u8);
        out.extend_from_slice(&slot.id);
        out.extend_from_slice(&slot.nonce);
        out.extend_from_slice(&slot.wrapped);
    }
    out.extend_from_slice(body);
    if out.len() > MAX_FILE_BYTES {
        return Err(StoreError::Limit("the file would be larger than 4 MiB"));
    }
    Ok(out)
}

fn body_aad() -> [u8; 6] {
    let mut aad = [0u8; 6];
    aad[..4].copy_from_slice(MAGIC);
    aad[4..].copy_from_slice(&VERSION.to_be_bytes());
    aad
}

fn slot_aad(kind: SlotKind, id: &[u8; ID_BYTES]) -> [u8; 6 + 1 + ID_BYTES] {
    let mut aad = [0u8; 6 + 1 + ID_BYTES];
    aad[..6].copy_from_slice(&body_aad());
    aad[6] = kind as u8;
    aad[7..].copy_from_slice(id);
    aad
}

/// `plain` sealed under `key` with a fresh random nonce: the nonce and the ciphertext with its tag.
fn seal(key: &[u8; KEY_BYTES], aad: &[u8], plain: &[u8]) -> std::result::Result<([u8; NONCE_BYTES], Vec<u8>), StoreError> {
    let nonce = random::<NONCE_BYTES>()?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let sealed = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .map_err(|_| StoreError::Limit("the data could not be encrypted"))?;
    Ok((nonce, sealed))
}

/// The plaintext of `sealed`, or [`StoreError::Locked`]: a wrong key and a changed byte are the same failure.
fn unseal(key: &[u8; KEY_BYTES], nonce: &[u8; NONCE_BYTES], aad: &[u8], sealed: &[u8]) -> std::result::Result<Zeroizing<Vec<u8>>, StoreError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(&XNonce::from(*nonce), Payload { msg: sealed, aad })
        .map(Zeroizing::new)
        .map_err(|_| StoreError::Locked)
}

fn wrap(dek: &Dek, kind: SlotKind, id: [u8; ID_BYTES], kek: &Kek) -> std::result::Result<Slot, StoreError> {
    let (nonce, sealed) = seal(kek.expose(), &slot_aad(kind, &id), &dek.0[..])?;
    let wrapped: [u8; WRAPPED_BYTES] = sealed.try_into().map_err(|_| StoreError::Format("a wrapped key has a wrong length"))?;
    Ok(Slot { kind, id, nonce, wrapped })
}

fn unwrap_dek(slot: &Slot, kek: &Kek) -> std::result::Result<Dek, StoreError> {
    let plain = unseal(kek.expose(), &slot.nonce, &slot_aad(slot.kind, &slot.id), &slot.wrapped)?;
    let bytes: &[u8; KEY_BYTES] = (&plain[..]).try_into().map_err(|_| StoreError::Locked)?;
    Ok(Dek::from_bytes(bytes))
}

/// The entries in a body, or [`StoreError::Locked`] if `dek` does not open it.
fn decode_body(dek: &Dek, body: &[u8]) -> std::result::Result<Entries, StoreError> {
    if body.len() < MIN_BODY_BYTES {
        return Err(StoreError::Format("the body is too short"));
    }
    let mut nonce = [0u8; NONCE_BYTES];
    nonce.copy_from_slice(&body[..NONCE_BYTES]);
    let plain = unseal(&dek.0, &nonce, &body_aad(), &body[NONCE_BYTES..])?;
    let entries: Entries = serde_json::from_slice(&plain).map_err(|_| StoreError::Format("the body is not a list of entries"))?;
    if entries.len() > MAX_ENTRIES {
        return Err(StoreError::Format("the body holds too many entries"));
    }
    Ok(entries)
}

/// A body for `entries`: a fresh nonce and the ciphertext.
fn encode_body(dek: &Dek, entries: &Entries) -> std::result::Result<Vec<u8>, StoreError> {
    let plain = Zeroizing::new(serde_json::to_vec(entries).map_err(|_| StoreError::Format("the entries could not be written"))?);
    let (nonce, sealed) = seal(&dek.0, &body_aad(), &plain)?;
    let mut body = Vec::with_capacity(NONCE_BYTES + sealed.len());
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&sealed);
    Ok(body)
}

fn check_account(account: &str) -> std::result::Result<(), StoreError> {
    if account.is_empty() || account.len() > MAX_LEN || account.chars().any(char::is_control) {
        return Err(StoreError::Invalid("the account of a secret is empty, too long or has a control character"));
    }
    Ok(())
}

fn check_secret(secret: &str) -> std::result::Result<(), StoreError> {
    if secret.len() > MAX_LEN {
        return Err(StoreError::Invalid("a secret is longer than 4096 bytes"));
    }
    Ok(())
}

fn nofollow() -> i32 {
    i32::try_from(OFlags::NOFOLLOW.bits()).expect("the open flags fit in an i32")
}

/// The whole file, refusing one over the cap and a link.
fn read_file(path: &Path) -> std::result::Result<Vec<u8>, StoreError> {
    let file = OpenOptions::new().read(true).custom_flags(nofollow()).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(StoreError::Format("it is not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(StoreError::Limit("the file is larger than 4 MiB"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(StoreError::Limit("the file is larger than 4 MiB"));
    }
    Ok(bytes)
}

/// The exclusive lock on the sidecar file; released when the value drops.
struct FileLock {
    _file: File,
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn lock_exclusive(path: &Path) -> std::result::Result<FileLock, StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nofollow())
        .open(sidecar(path, ".lock"))?;
    loop {
        match rustix::fs::flock(&file, FlockOperation::LockExclusive) {
            Ok(()) => return Ok(FileLock { _file: file }),
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(StoreError::Io(io::Error::from(e))),
        }
    }
}

/// The prefix of the temporary files of `path`.
fn temp_prefix(path: &Path) -> String {
    let mut name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.push_str(".tmp-");
    name
}

fn directory_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Write `bytes` as the new `path`: a temporary file next to it (0600, exclusive creation), synced, renamed over it, the
/// directory synced. The caller holds the exclusive lock.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::result::Result<(), StoreError> {
    let suffix: String = random::<8>()?.iter().map(|b| format!("{b:02x}")).collect();
    let temp = directory_of(path).join(format!("{}{suffix}", temp_prefix(path)));
    let outcome = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(nofollow()).open(&temp)?;
        file.set_permissions(Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        File::open(directory_of(path))?.sync_all()
    })();
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&temp);
        return Err(StoreError::Io(e));
    }
    Ok(())
}

/// Remove the temporary files a crash left next to `path`. The caller holds the exclusive lock, so no writer is using one.
fn remove_stray_temporaries(path: &Path) -> std::result::Result<(), StoreError> {
    let prefix = temp_prefix(path);
    for item in std::fs::read_dir(directory_of(path))? {
        let item = item?;
        if !item.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        if item.file_type()?.is_file() {
            std::fs::remove_file(item.path())?;
        }
    }
    Ok(())
}

enum State {
    Locked,
    Unlocked(Dek),
}

/// The secret store over one encrypted file. It starts locked (after [`open`](Self::open)) or unlocked (after
/// [`create`](Self::create)).
pub struct EncryptedFileStore {
    path: PathBuf,
    state: Mutex<State>,
}

impl fmt::Debug for EncryptedFileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptedFileStore(..)")
    }
}

impl EncryptedFileStore {
    /// Make a new store file at `path` (mode 0600) with one slot and no entries, and leave it unlocked. A store at that path
    /// already is refused. The directory must exist.
    pub fn create(path: &Path, kind: SlotKind, slot_id: [u8; ID_BYTES], kek: &Kek) -> std::result::Result<EncryptedFileStore, StoreError> {
        let _lock = lock_exclusive(path)?;
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(StoreError::Exists);
        }
        let dek = Dek::random()?;
        let slot = wrap(&dek, kind, slot_id, kek)?;
        let body = encode_body(&dek, &Entries::new())?;
        write_atomic(path, &serialize(&[slot], &body)?)?;
        Ok(EncryptedFileStore { path: path.to_path_buf(), state: Mutex::new(State::Unlocked(dek)) })
    }

    /// Open the store at `path`, locked. A temporary file a crash left is removed; the store itself is not changed.
    pub fn open(path: &Path) -> std::result::Result<EncryptedFileStore, StoreError> {
        let _lock = lock_exclusive(path)?;
        remove_stray_temporaries(path)?;
        parse(&read_file(path)?)?;
        Ok(EncryptedFileStore { path: path.to_path_buf(), state: Mutex::new(State::Locked) })
    }

    /// Unlock with the key of a slot of `kind`. A wrong key, a kind with no slot and a damaged slot all give [`LOCKED`]. A
    /// failed unlock changes neither the file nor the state.
    pub fn unlock(&self, kind: SlotKind, kek: &Kek) -> std::result::Result<(), StoreError> {
        let parsed = parse(&read_file(&self.path)?)?;
        for slot in parsed.slots.iter().filter(|s| s.kind == kind) {
            if let Ok(dek) = unwrap_dek(slot, kek)
                && decode_body(&dek, &parsed.body).is_ok()
            {
                *self.state() = State::Unlocked(dek);
                return Ok(());
            }
        }
        Err(StoreError::Locked)
    }

    /// Lock the store: the data key is wiped from memory.
    pub fn lock(&self) {
        *self.state() = State::Locked;
    }

    /// Whether the store holds its data key.
    pub fn is_unlocked(&self) -> bool {
        matches!(*self.state(), State::Unlocked(_))
    }

    /// The kinds and ids of the slots in the file. This needs no key.
    pub fn slots(&self) -> std::result::Result<Vec<(SlotKind, [u8; ID_BYTES])>, StoreError> {
        Ok(parse(&read_file(&self.path)?)?.slots.iter().map(|s| (s.kind, s.id)).collect())
    }

    /// Add a slot that `kek` opens. The entries are not re-encrypted. A slot of that kind and id exists: refused.
    pub fn add_slot(&self, kind: SlotKind, id: [u8; ID_BYTES], kek: &Kek) -> std::result::Result<(), StoreError> {
        self.change_slots(|dek, slots| {
            if slots.iter().any(|s| s.kind == kind && s.id == id) {
                return Err(StoreError::Slot("a slot with that kind and id exists"));
            }
            if slots.len() >= MAX_SLOTS {
                return Err(StoreError::Slot("a store holds at most 16 slots"));
            }
            slots.push(wrap(dek, kind, id, kek)?);
            Ok(())
        })
    }

    /// Wrap the data key again under a new key in the slot of that kind and id. The entries are not re-encrypted.
    pub fn replace_slot(&self, kind: SlotKind, id: [u8; ID_BYTES], kek: &Kek) -> std::result::Result<(), StoreError> {
        self.change_slots(|dek, slots| {
            let slot = slots.iter_mut().find(|s| s.kind == kind && s.id == id).ok_or(StoreError::Slot("there is no such slot"))?;
            *slot = wrap(dek, kind, id, kek)?;
            Ok(())
        })
    }

    /// Remove a slot. The last slot cannot be removed, because nothing could open the store afterwards.
    pub fn remove_slot(&self, kind: SlotKind, id: [u8; ID_BYTES]) -> std::result::Result<(), StoreError> {
        self.change_slots(|_, slots| {
            let index = slots.iter().position(|s| s.kind == kind && s.id == id).ok_or(StoreError::Slot("there is no such slot"))?;
            if slots.len() == 1 {
                return Err(StoreError::Slot("the last slot cannot be removed"));
            }
            slots.remove(index);
            Ok(())
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the file with the data key, check that the key opens the body, and return what is in it with the key held.
    fn read_entries(&self, state: &State) -> std::result::Result<(Parsed, Entries), StoreError> {
        let State::Unlocked(dek) = state else {
            return Err(StoreError::Locked);
        };
        let parsed = parse(&read_file(&self.path)?)?;
        let entries = decode_body(dek, &parsed.body)?;
        Ok((parsed, entries))
    }

    /// A change of the slot table under the exclusive lock; the body is carried over byte for byte.
    fn change_slots(
        &self,
        change: impl FnOnce(&Dek, &mut Vec<Slot>) -> std::result::Result<(), StoreError>,
    ) -> std::result::Result<(), StoreError> {
        let state = self.state();
        if !matches!(*state, State::Unlocked(_)) {
            return Err(StoreError::Locked);
        }
        let _lock = lock_exclusive(&self.path)?;
        let (mut parsed, _entries) = self.read_entries(&state)?;
        let State::Unlocked(dek) = &*state else {
            return Err(StoreError::Locked);
        };
        change(dek, &mut parsed.slots)?;
        write_atomic(&self.path, &serialize(&parsed.slots, &parsed.body)?)
    }

    /// A change of the entries under the exclusive lock. `change` returns whether the entries changed.
    fn change_entries(&self, change: impl FnOnce(&mut Entries) -> std::result::Result<bool, StoreError>) -> std::result::Result<(), StoreError> {
        let state = self.state();
        if !matches!(*state, State::Unlocked(_)) {
            return Err(StoreError::Locked);
        }
        let _lock = lock_exclusive(&self.path)?;
        let (parsed, mut entries) = self.read_entries(&state)?;
        if !change(&mut entries)? {
            return Ok(());
        }
        let State::Unlocked(dek) = &*state else {
            return Err(StoreError::Locked);
        };
        let body = encode_body(dek, &entries)?;
        write_atomic(&self.path, &serialize(&parsed.slots, &body)?)
    }

    fn get_inner(&self, account: &str) -> std::result::Result<Option<String>, StoreError> {
        let state = self.state();
        if !matches!(*state, State::Unlocked(_)) {
            return Err(StoreError::Locked);
        }
        check_account(account)?;
        let (_, entries) = self.read_entries(&state)?;
        Ok(entries.get(account).map(|e| e.0.clone()))
    }

    fn set_inner(&self, account: &str, secret: &str) -> std::result::Result<(), StoreError> {
        if !self.is_unlocked() {
            return Err(StoreError::Locked);
        }
        check_account(account)?;
        check_secret(secret)?;
        self.change_entries(|entries| {
            if !entries.contains_key(account) && entries.len() >= MAX_ENTRIES {
                return Err(StoreError::Limit("a store holds at most 4096 entries"));
            }
            entries.insert(account.to_owned(), Entry(secret.to_owned()));
            Ok(true)
        })
    }

    fn delete_inner(&self, account: &str) -> std::result::Result<(), StoreError> {
        if !self.is_unlocked() {
            return Err(StoreError::Locked);
        }
        check_account(account)?;
        self.change_entries(|entries| Ok(entries.remove(account).is_some()))
    }
}

impl SecretStore for EncryptedFileStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.get_inner(account)?)
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        Ok(self.set_inner(account, secret)?)
    }

    fn delete(&self, account: &str) -> Result<()> {
        Ok(self.delete_inner(account)?)
    }
}
