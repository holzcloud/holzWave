//! The secret store of Android, as plain Rust that is tested on the desktop.
//!
//! The `keyring` crate has no Android backend, so secrets (SSH keys, passwords, sync keys, AI provider keys) live in a
//! [`VaultStore`]: one file that holds ciphertext only. A random 256 bit data key seals every secret with
//! XChaCha20-Poly1305 (the AEAD of `chacha20poly1305`, as in the sync format) under a fresh 24 byte nonce for each write.
//! The data key itself is wrapped by a key that never leaves the device's secure hardware; the [`Keystore`] trait is
//! that hardware. The Kotlin side of the trait runs the Android Keystore, and [`FakeKeystore`] stands in for it on the
//! desktop, so everything but the platform call is proven without a phone.
//!
//! File format, version 1 (all numbers big endian):
//!
//! ```text
//! "HWV" | version u8 | protection u8 | wrapped length u16 | wrapped data key | nonce [u8; 24] | ciphertext + tag [u8; 16]
//! ```
//!
//! Everything up to the nonce is the header, and the header is the associated data of the seal, so a changed byte of the
//! header, the wrapped key, the nonce or the ciphertext ends in the state `lost` with the reason `corrupt` and never in an
//! empty store. All accounts and secrets are one JSON object inside the one sealed blob, so the file shows neither.
//!
//! The vault has four states. `new`: no file yet. `locked`: a file whose data key has not been unwrapped in this session.
//! `unlocked`: the data key is in memory. `lost`: the key is gone (the screen lock was removed) or the file is damaged.
//! There are two protections. `auth`: the wrapping key needs the screen lock or a biometric for every unwrap, so an
//! unlock is one system prompt per session. `plain`: the wrapping key needs no prompt (a device without a screen lock),
//! and the vault opens unlocked. A vault that is not unlocked answers `get`, `set` and `delete` with the one error text
//! [`LOCKED_TEXT`]. An unlocked auth vault locks again after [`BACKGROUND_LOCK_MS`] with the page hidden, measured on an
//! injected [`Clock`], and the lock zeroizes the data key.
//!
//! A vault never prints, logs or reports a secret: `Debug` shows the state and the protection, every error text is a
//! fixed phrase, and the file is ciphertext.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::persist;
use crate::secrets::{MAX_LEN, SecretStore};
use crate::sync::crypto::random;
use crate::{Error, Result};

/// The text of the error of every refused operation of a vault that is not unlocked. The host's `is_locked` matches an
/// I/O error whose text is exactly this.
pub const LOCKED_TEXT: &str = "secrets are locked";

/// How long the page may be hidden before an unlocked auth vault locks again: five minutes.
pub const BACKGROUND_LOCK_MS: u64 = 300_000;

/// The first bytes of the file.
pub const MAGIC: &[u8; 3] = b"HWV";

/// The format version this code writes and reads.
pub const VERSION: u8 = 1;

/// The largest vault file in bytes; a larger one is refused before it is read.
pub const MAX_FILE: u64 = 8 * 1024 * 1024;

/// The most accounts a vault holds.
pub const MAX_ENTRIES: usize = 4096;

/// The longest wrapped data key that is accepted from a file.
const MAX_WRAPPED: usize = 1024;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Magic, version, protection, wrapped length.
const FIXED_HEADER: usize = 3 + 1 + 1 + 2;

type Map = BTreeMap<String, Secret>;
type Key = Zeroizing<[u8; KEY_LEN]>;

/// A secret as it sits in the decrypted map: wiped when dropped, never printed.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Secret(String);

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

fn err(message: &str) -> Error {
    Error::Io(io::Error::other(message.to_owned()))
}

/// The error of a vault that is not unlocked, in the shape the host's `is_locked` expects: an I/O error whose text is
/// exactly [`LOCKED_TEXT`].
fn locked() -> Error {
    Error::Io(io::Error::other(LOCKED_TEXT))
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The same rule as the core's other secret stores.
fn check(account: &str, secret: Option<&str>) -> Result<()> {
    if account.is_empty() || account.len() > MAX_LEN || account.chars().any(char::is_control) {
        return Err(Error::Unsupported("the account of a secret is empty, too long or has a control character".into()));
    }
    if secret.is_some_and(|s| s.len() > MAX_LEN) {
        return Err(Error::Unsupported(format!("a secret is longer than {MAX_LEN} bytes")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// The keystore
// ---------------------------------------------------------------------------------------------------------------------

/// What protects the wrapping key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    /// The key needs the screen lock or a biometric for every use.
    Auth,
    /// The key needs nothing: a device without a screen lock.
    Plain,
}

impl Protection {
    /// The name the page gets: `auth` or `plain`.
    pub fn as_str(self) -> &'static str {
        match self {
            Protection::Auth => "auth",
            Protection::Plain => "plain",
        }
    }

    fn byte(self) -> u8 {
        match self {
            Protection::Auth => 1,
            Protection::Plain => 2,
        }
    }

    fn from_byte(byte: u8) -> Option<Protection> {
        match byte {
            1 => Some(Protection::Auth),
            2 => Some(Protection::Plain),
            _ => None,
        }
    }
}

/// What the device can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeystoreCaps {
    /// The device has a screen lock, so an auth key can be made.
    pub has_screen_lock: bool,
    /// The key is held by a separate secure chip.
    pub strongbox: bool,
}

/// Why a keystore call failed. No text holds a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeystoreError {
    /// The person cancelled the system prompt.
    Cancelled,
    /// The wrapping key does not exist any more (the screen lock was removed).
    KeyGone,
    /// An auth key was asked for on a device without a screen lock.
    NoScreenLock,
    /// The bytes given to `unwrap` were not made by this keystore, or were changed.
    Invalid,
    /// Anything else, with a short text of no secret.
    Failed(String),
}

impl fmt::Display for KeystoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeystoreError::Cancelled => f.write_str("the prompt was cancelled"),
            KeystoreError::KeyGone => f.write_str("the key of the secret store is gone"),
            KeystoreError::NoScreenLock => f.write_str("the device has no screen lock"),
            KeystoreError::Invalid => f.write_str("the wrapped key was not made by this device key or was changed"),
            KeystoreError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for KeystoreError {}

/// The one wrapping key of the device and the calls that use it.
///
/// `wrap` and `unwrap` under an auth key may block while the system shows a prompt: callers use a worker thread, never
/// the UI thread. `caps` never blocks and never prompts.
///
/// Keys are added, never replaced in place: [`generate`](Keystore::generate) makes a new key and makes it the one `wrap`
/// uses, while the keys of earlier wrapped blobs stay usable by `unwrap` until [`retire_previous`](Keystore::retire_previous).
/// That is what lets a switch of protection write the new file before the old key goes.
pub trait Keystore: Send + Sync {
    /// What the device can do right now.
    fn caps(&self) -> KeystoreCaps;
    /// Make a new wrapping key with this protection and make it the one `wrap` uses.
    fn generate(&self, protection: Protection) -> std::result::Result<(), KeystoreError>;
    /// Wrap `plain` with the current wrapping key.
    fn wrap(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, KeystoreError>;
    /// Unwrap what an earlier `wrap` returned.
    fn unwrap(&self, wrapped: &[u8]) -> std::result::Result<Zeroizing<Vec<u8>>, KeystoreError>;
    /// Remove every wrapping key but the current one.
    fn retire_previous(&self) -> std::result::Result<(), KeystoreError>;
    /// Remove every wrapping key.
    fn delete(&self) -> std::result::Result<(), KeystoreError>;
}

struct FakeKey {
    id: u8,
    key: Key,
    protection: Protection,
}

struct FakeInner {
    keys: Vec<FakeKey>,
    current: Option<u8>,
    next_id: u8,
    screen_lock: bool,
    strongbox: bool,
    cancel_next: bool,
    prompts: u32,
}

/// A keystore in memory, for tests: the same AEAD with its own random keys, a count of the prompts an auth key would
/// have shown, and switches to cancel the next prompt, to lose the keys and to report no screen lock. It proves the
/// logic of the vault, not the platform.
pub struct FakeKeystore {
    inner: Mutex<FakeInner>,
}

impl Default for FakeKeystore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FakeKeystore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let g = guard(&self.inner);
        f.debug_struct("FakeKeystore").field("keys", &g.keys.len()).field("prompts", &g.prompts).finish()
    }
}

impl FakeKeystore {
    /// A keystore with a screen lock and no StrongBox.
    pub fn new() -> FakeKeystore {
        FakeKeystore {
            inner: Mutex::new(FakeInner {
                keys: Vec::new(),
                current: None,
                next_id: 0,
                screen_lock: true,
                strongbox: false,
                cancel_next: false,
                prompts: 0,
            }),
        }
    }

    /// A keystore that reports no screen lock.
    pub fn without_screen_lock() -> FakeKeystore {
        let fake = FakeKeystore::new();
        fake.set_screen_lock(false);
        fake
    }

    /// Whether the device reports a screen lock from now on.
    pub fn set_screen_lock(&self, on: bool) {
        guard(&self.inner).screen_lock = on;
    }

    /// Whether the device reports StrongBox from now on.
    pub fn set_strongbox(&self, on: bool) {
        guard(&self.inner).strongbox = on;
    }

    /// The next prompt (a `wrap` or `unwrap` under an auth key) is cancelled.
    pub fn cancel_next_prompt(&self) {
        guard(&self.inner).cancel_next = true;
    }

    /// Every wrapping key is gone, as when the screen lock is removed.
    pub fn lose_key(&self) {
        let mut g = guard(&self.inner);
        g.keys.clear();
        g.current = None;
    }

    /// How many times `wrap` or `unwrap` ran under an auth key, cancelled prompts included.
    pub fn prompts(&self) -> u32 {
        guard(&self.inner).prompts
    }

    /// How many wrapping keys exist.
    pub fn key_count(&self) -> usize {
        guard(&self.inner).keys.len()
    }

    fn prompt(g: &mut FakeInner, protection: Protection) -> std::result::Result<(), KeystoreError> {
        if protection == Protection::Auth {
            g.prompts += 1;
            if std::mem::take(&mut g.cancel_next) {
                return Err(KeystoreError::Cancelled);
            }
        }
        Ok(())
    }
}

fn fake_cipher(key: &Key) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new((&**key).into())
}

impl Keystore for FakeKeystore {
    fn caps(&self) -> KeystoreCaps {
        let g = guard(&self.inner);
        KeystoreCaps { has_screen_lock: g.screen_lock, strongbox: g.strongbox }
    }

    fn generate(&self, protection: Protection) -> std::result::Result<(), KeystoreError> {
        let mut g = guard(&self.inner);
        if protection == Protection::Auth && !g.screen_lock {
            return Err(KeystoreError::NoScreenLock);
        }
        let bytes: [u8; KEY_LEN] = random().map_err(|_| KeystoreError::Failed("no random bytes".into()))?;
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.keys.retain(|k| k.id != id);
        g.keys.push(FakeKey { id, key: Zeroizing::new(bytes), protection });
        g.current = Some(id);
        Ok(())
    }

    fn wrap(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, KeystoreError> {
        let mut g = guard(&self.inner);
        let id = g.current.ok_or(KeystoreError::KeyGone)?;
        let position = g.keys.iter().position(|k| k.id == id).ok_or(KeystoreError::KeyGone)?;
        let protection = g.keys[position].protection;
        Self::prompt(&mut g, protection)?;
        let nonce: [u8; NONCE_LEN] = random().map_err(|_| KeystoreError::Failed("no random bytes".into()))?;
        let sealed = fake_cipher(&g.keys[position].key)
            .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad: &[id] })
            .map_err(|_| KeystoreError::Failed("wrap failed".into()))?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
        out.push(id);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    fn unwrap(&self, wrapped: &[u8]) -> std::result::Result<Zeroizing<Vec<u8>>, KeystoreError> {
        if wrapped.len() < 1 + NONCE_LEN + TAG_LEN {
            return Err(KeystoreError::Invalid);
        }
        let mut g = guard(&self.inner);
        let id = wrapped[0];
        // No key at all is a lost key. A key that is not the one named is bytes this keystore did not make.
        let position = match g.keys.iter().position(|k| k.id == id) {
            Some(position) => position,
            None if g.keys.is_empty() => return Err(KeystoreError::KeyGone),
            None => return Err(KeystoreError::Invalid),
        };
        let protection = g.keys[position].protection;
        Self::prompt(&mut g, protection)?;
        let nonce = XNonce::try_from(&wrapped[1..1 + NONCE_LEN]).map_err(|_| KeystoreError::Invalid)?;
        let plain = fake_cipher(&g.keys[position].key)
            .decrypt(&nonce, Payload { msg: &wrapped[1 + NONCE_LEN..], aad: &[id] })
            .map_err(|_| KeystoreError::Invalid)?;
        Ok(Zeroizing::new(plain))
    }

    fn retire_previous(&self) -> std::result::Result<(), KeystoreError> {
        let mut g = guard(&self.inner);
        let current = g.current;
        g.keys.retain(|k| Some(k.id) == current);
        Ok(())
    }

    fn delete(&self) -> std::result::Result<(), KeystoreError> {
        let mut g = guard(&self.inner);
        g.keys.clear();
        g.current = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------------------------------------------------

/// Monotonic milliseconds.
///
/// The count must also run while the device is suspended, or a phone that sleeps in a pocket would stay unlocked for
/// hours. The Android clock (boot time) does; [`SystemClock`] does not.
pub trait Clock: Send + Sync {
    /// Milliseconds on a clock that never goes back.
    fn now_ms(&self) -> u64;
}

/// The desktop clock, built on [`Instant`]. It does not count time in which the machine was suspended.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self { start: Instant::now() }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A clock a test moves by hand.
#[derive(Debug, Default)]
pub struct FakeClock {
    ms: AtomicU64,
}

impl FakeClock {
    /// A clock at `ms`.
    pub fn new(ms: u64) -> FakeClock {
        FakeClock { ms: AtomicU64::new(ms) }
    }

    /// Put the clock at `ms`, forward or back.
    pub fn set(&self, ms: u64) {
        self.ms.store(ms, Ordering::SeqCst);
    }

    /// Move the clock forward by `ms`.
    pub fn advance(&self, ms: u64) {
        self.ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------------------------------------------------

/// Where the one file of the vault is read and written. [`DiskFiles`] is the real one; [`MemoryFiles`] is for tests.
pub trait VaultFiles: Send + Sync {
    /// The file, `None` if it does not exist. A file larger than `max` is an error and is not read.
    fn read(&self, path: &Path, max: u64) -> io::Result<Option<Vec<u8>>>;
    /// Replace the file so that a failure leaves the old content (an atomic write).
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Remove the file; one that does not exist is not an error.
    fn remove(&self, path: &Path) -> io::Result<()>;
}

/// The file system: bounded reads and private atomic writes of [`persist`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskFiles;

impl VaultFiles for DiskFiles {
    fn read(&self, path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
        persist::read_limited(path, max)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        persist::write_atomic_private(path, bytes)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// One file in memory, for tests: the content, a switch that makes every write fail, and a count of the writes.
#[derive(Debug, Default)]
pub struct MemoryFiles {
    content: Mutex<Option<Vec<u8>>>,
    fail_writes: AtomicBool,
    writes: AtomicU64,
}

impl MemoryFiles {
    /// The bytes of the file, if there is one.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        guard(&self.content).clone()
    }

    /// Put these bytes in the file, as another process could.
    pub fn replace(&self, bytes: Vec<u8>) {
        *guard(&self.content) = Some(bytes);
    }

    /// Whether every write fails from now on.
    pub fn fail_writes(&self, on: bool) {
        self.fail_writes.store(on, Ordering::SeqCst);
    }

    /// How many writes succeeded.
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }
}

impl VaultFiles for MemoryFiles {
    fn read(&self, _path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
        let content = guard(&self.content);
        match content.as_ref() {
            Some(bytes) if bytes.len() as u64 > max => Err(io::Error::new(io::ErrorKind::InvalidData, "file too large")),
            other => Ok(other.cloned()),
        }
    }

    fn write(&self, _path: &Path, bytes: &[u8]) -> io::Result<()> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(io::Error::other("the rename was refused"));
        }
        *guard(&self.content) = Some(bytes.to_vec());
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn remove(&self, _path: &Path) -> io::Result<()> {
        *guard(&self.content) = None;
        Ok(())
    }
}

/// A file read and checked as far as it can be without the data key.
struct Parsed {
    protection: Protection,
    wrapped: Vec<u8>,
    bytes: Vec<u8>,
    header_end: usize,
}

impl Parsed {
    fn parse(bytes: Vec<u8>) -> Option<Parsed> {
        if bytes.len() < FIXED_HEADER || &bytes[..3] != MAGIC || bytes[3] != VERSION {
            return None;
        }
        let protection = Protection::from_byte(bytes[4])?;
        let wrapped_len = usize::from(u16::from_be_bytes([bytes[5], bytes[6]]));
        if wrapped_len == 0 || wrapped_len > MAX_WRAPPED {
            return None;
        }
        let header_end = FIXED_HEADER + wrapped_len;
        // The body is a nonce, at least `{}`, and a tag.
        if bytes.len() < header_end + NONCE_LEN + 2 + TAG_LEN || bytes.len() as u64 > MAX_FILE {
            return None;
        }
        let wrapped = bytes[FIXED_HEADER..header_end].to_vec();
        Some(Parsed { protection, wrapped, bytes, header_end })
    }

    /// The associated data of the seal: everything before the nonce.
    fn header(&self) -> &[u8] {
        &self.bytes[..self.header_end]
    }

    /// The decrypted body, `None` if it does not authenticate under `key`.
    fn open(&self, key: &Key) -> Option<Zeroizing<Vec<u8>>> {
        let nonce = XNonce::try_from(&self.bytes[self.header_end..self.header_end + NONCE_LEN]).ok()?;
        let plain = fake_cipher(key)
            .decrypt(&nonce, Payload { msg: &self.bytes[self.header_end + NONCE_LEN..], aad: self.header() })
            .ok()?;
        Some(Zeroizing::new(plain))
    }
}

enum Read {
    Missing,
    Bad,
    Found(Parsed),
}

fn read_parsed(files: &dyn VaultFiles, path: &Path) -> Read {
    match files.read(path, MAX_FILE) {
        Ok(None) => Read::Missing,
        Ok(Some(bytes)) => Parsed::parse(bytes).map_or(Read::Bad, Read::Found),
        Err(_) => Read::Bad,
    }
}

/// The whole file for `map`: the header, a fresh nonce and the sealed blob.
fn seal(protection: Protection, wrapped: &[u8], key: &Key, map: &Map) -> Result<Vec<u8>> {
    let wrapped_len = u16::try_from(wrapped.len()).map_err(|_| err("the wrapped key is too long"))?;
    let plain = Zeroizing::new(serde_json::to_vec(map).map_err(|_| err("the secrets could not be encoded"))?);
    let mut out = Vec::with_capacity(FIXED_HEADER + wrapped.len() + NONCE_LEN + plain.len() + TAG_LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(protection.byte());
    out.extend_from_slice(&wrapped_len.to_be_bytes());
    out.extend_from_slice(wrapped);
    let nonce: [u8; NONCE_LEN] = random()?;
    out.extend_from_slice(&nonce);
    let sealed = fake_cipher(key)
        .encrypt(&XNonce::from(nonce), Payload { msg: &plain, aad: &out[..FIXED_HEADER + wrapped.len()] })
        .map_err(|_| err("encryption failed"))?;
    out.extend_from_slice(&sealed);
    if out.len() as u64 > MAX_FILE {
        return Err(Error::Unsupported("the secrets are larger than the vault file may be".into()));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------------------------------------------------

/// Why a vault is lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LostReason {
    /// The wrapping key does not exist any more.
    KeyGone,
    /// The file is damaged, changed, too large or gone.
    Corrupt,
}

impl LostReason {
    /// The name the page gets: `key_gone` or `corrupt`.
    pub fn as_str(self) -> &'static str {
        match self {
            LostReason::KeyGone => "key_gone",
            LostReason::Corrupt => "corrupt",
        }
    }
}

/// The four states of a vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultState {
    New,
    Locked,
    Unlocked,
    Lost,
}

impl VaultState {
    /// The name the page gets: `new`, `locked`, `unlocked` or `lost`.
    pub fn as_str(self) -> &'static str {
        match self {
            VaultState::New => "new",
            VaultState::Locked => "locked",
            VaultState::Unlocked => "unlocked",
            VaultState::Lost => "lost",
        }
    }
}

/// A notice for the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// Nothing to say.
    None,
    /// The device has no screen lock, so the secrets are protected by the device key only.
    NoLock,
}

impl Notice {
    /// The name the page gets: `none` or `no_lock`.
    pub fn as_str(self) -> &'static str {
        match self {
            Notice::None => "none",
            Notice::NoLock => "no_lock",
        }
    }
}

/// What the page is told about the vault. It never holds a secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VaultStatus {
    pub state: VaultState,
    pub protection: Protection,
    pub has_screen_lock: bool,
    pub strongbox: bool,
    /// A plain vault can be switched to auth now.
    pub can_protect: bool,
    pub notice: Notice,
    /// Why the vault is lost, if it is.
    pub reason: Option<LostReason>,
}

/// Why an operation of the vault failed. No text holds a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultError {
    /// The person cancelled the system prompt; nothing changed.
    Cancelled,
    /// The device has no screen lock, so the auth protection cannot be used.
    NoScreenLock,
    /// The vault is lost; it is reset on request.
    Lost(LostReason),
    /// Anything else, with a short text of no secret.
    Failed(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::Cancelled => f.write_str("the unlock was cancelled"),
            VaultError::NoScreenLock => f.write_str("the device has no screen lock"),
            VaultError::Lost(LostReason::KeyGone) => f.write_str("the secret store is lost: its key is gone"),
            VaultError::Lost(LostReason::Corrupt) => f.write_str("the secret store is lost: its file is damaged"),
            VaultError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<KeystoreError> for VaultError {
    fn from(e: KeystoreError) -> Self {
        match e {
            KeystoreError::Cancelled => VaultError::Cancelled,
            KeystoreError::NoScreenLock => VaultError::NoScreenLock,
            KeystoreError::KeyGone => VaultError::Lost(LostReason::KeyGone),
            KeystoreError::Invalid => VaultError::Lost(LostReason::Corrupt),
            KeystoreError::Failed(m) => VaultError::Failed(m),
        }
    }
}

impl From<Error> for VaultError {
    fn from(e: Error) -> Self {
        // The text of an I/O error is about the file system, never about a secret.
        VaultError::Failed(e.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    New,
    Locked,
    Unlocked,
    Lost(LostReason),
}

struct Inner {
    phase: Phase,
    protection: Protection,
    wrapped: Vec<u8>,
    key: Option<Key>,
    hidden_since: Option<u64>,
    last_now: u64,
}

impl Inner {
    /// Drop the data key (it is zeroed) and, if the vault was unlocked, lock it.
    fn lock(&mut self) {
        self.key = None;
        self.hidden_since = None;
        if self.phase == Phase::Unlocked {
            self.phase = Phase::Locked;
        }
    }

    fn lose(&mut self, reason: LostReason) {
        self.key = None;
        self.hidden_since = None;
        self.phase = Phase::Lost(reason);
    }
}

fn default_protection(caps: KeystoreCaps) -> Protection {
    if caps.has_screen_lock { Protection::Auth } else { Protection::Plain }
}

/// The secret store of Android. See the module text.
pub struct VaultStore {
    path: PathBuf,
    keystore: Arc<dyn Keystore>,
    clock: Arc<dyn Clock>,
    files: Arc<dyn VaultFiles>,
    inner: Mutex<Inner>,
    /// Serialises the operations that call the keystore, which may block on a prompt. Never taken by `get`, `set`,
    /// `status`, `lock`, `hidden` or `tick`, so none of them waits for a prompt.
    ops: Mutex<()>,
    /// How many `unlock` calls have finished, and the answer of the last one: a call that waited for another shares it.
    unlocks: AtomicU64,
    last_unlock: Mutex<std::result::Result<(), VaultError>>,
}

impl fmt::Debug for VaultStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.inner.try_lock() {
            Ok(g) => {
                let state = state_of(g.phase);
                f.debug_struct("VaultStore").field("state", &state).field("protection", &g.protection).finish_non_exhaustive()
            }
            Err(_) => f.write_str("VaultStore { busy }"),
        }
    }
}

fn state_of(phase: Phase) -> VaultState {
    match phase {
        Phase::New => VaultState::New,
        Phase::Locked => VaultState::Locked,
        Phase::Unlocked => VaultState::Unlocked,
        Phase::Lost(_) => VaultState::Lost,
    }
}

impl VaultStore {
    /// The vault in the file `path`, on the file system. Reads nothing secret: the file is looked at (its size first,
    /// then its header) and the state follows. A plain vault is unwrapped at once (no prompt) and opens unlocked; an
    /// auth vault opens locked.
    pub fn open(path: impl Into<PathBuf>, keystore: Arc<dyn Keystore>, clock: Arc<dyn Clock>) -> VaultStore {
        Self::open_with_files(path, keystore, clock, Arc::new(DiskFiles))
    }

    /// [`open`](VaultStore::open) on another file source.
    pub fn open_with_files(
        path: impl Into<PathBuf>,
        keystore: Arc<dyn Keystore>,
        clock: Arc<dyn Clock>,
        files: Arc<dyn VaultFiles>,
    ) -> VaultStore {
        let path = path.into();
        let caps = keystore.caps();
        let (phase, protection, wrapped) = match read_parsed(&*files, &path) {
            Read::Missing => (Phase::New, default_protection(caps), Vec::new()),
            Read::Bad => (Phase::Lost(LostReason::Corrupt), default_protection(caps), Vec::new()),
            Read::Found(parsed) => (Phase::Locked, parsed.protection, parsed.wrapped),
        };
        let last_now = clock.now_ms();
        let store = VaultStore {
            path,
            keystore,
            clock,
            files,
            inner: Mutex::new(Inner { phase, protection, wrapped, key: None, hidden_since: None, last_now }),
            ops: Mutex::new(()),
            unlocks: AtomicU64::new(0),
            last_unlock: Mutex::new(Ok(())),
        };
        if phase == Phase::Locked && protection == Protection::Plain {
            // No prompt. A failure leaves the vault locked or lost, as `unlock` decides.
            drop(store.unlock());
        }
        store
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        guard(&self.inner)
    }

    /// Lock an unlocked auth vault whose page has been hidden for five minutes or whose clock went back. A plain vault
    /// never locks on a clock.
    fn expire(&self, g: &mut Inner, now: u64) {
        if g.protection != Protection::Auth || g.phase != Phase::Unlocked {
            return;
        }
        let backwards = now < g.last_now;
        g.last_now = now;
        let expired = g.hidden_since.is_some_and(|since| now.saturating_sub(since) >= BACKGROUND_LOCK_MS);
        if backwards || expired {
            g.lock();
        }
    }

    /// The state now.
    pub fn state(&self) -> VaultState {
        let mut g = self.inner();
        self.expire(&mut g, self.clock.now_ms());
        state_of(g.phase)
    }

    /// Why the vault is lost, `None` in every other state.
    pub fn lost_reason(&self) -> Option<LostReason> {
        let mut g = self.inner();
        self.expire(&mut g, self.clock.now_ms());
        match g.phase {
            Phase::Lost(reason) => Some(reason),
            _ => None,
        }
    }

    /// What the page is told. Never prompts and never waits for a prompt.
    pub fn status(&self) -> VaultStatus {
        let caps = self.keystore.caps();
        let mut g = self.inner();
        self.expire(&mut g, self.clock.now_ms());
        let protection = if g.phase == Phase::New { default_protection(caps) } else { g.protection };
        let usable = matches!(g.phase, Phase::Locked | Phase::Unlocked);
        VaultStatus {
            state: state_of(g.phase),
            protection,
            has_screen_lock: caps.has_screen_lock,
            strongbox: caps.strongbox,
            can_protect: usable && protection == Protection::Plain && caps.has_screen_lock,
            notice: if protection == Protection::Plain && !caps.has_screen_lock { Notice::NoLock } else { Notice::None },
            reason: match g.phase {
                Phase::Lost(reason) => Some(reason),
                _ => None,
            },
        }
    }

    /// Lock at once; the data key is zeroed.
    pub fn lock(&self) {
        self.inner().lock();
    }

    /// The page was hidden (`true`) or shown again (`false`). An unlocked auth vault locks when it was hidden for
    /// [`BACKGROUND_LOCK_MS`]; a second `true` without a `false` between does not move the start.
    pub fn hidden(&self, hidden: bool) {
        let now = self.clock.now_ms();
        let mut g = self.inner();
        self.expire(&mut g, now);
        if hidden {
            if g.hidden_since.is_none() {
                g.hidden_since = Some(now);
            }
        } else {
            g.hidden_since = None;
        }
    }

    /// For a timer of the host: lock if the five minutes have passed while the page is still hidden, or if the clock
    /// went back.
    pub fn tick(&self) {
        let now = self.clock.now_ms();
        let mut g = self.inner();
        self.expire(&mut g, now);
    }

    /// Unlock: one system prompt for an auth vault, none for a plain one. In the `new` state this creates the vault.
    /// May block while the prompt is shown: call it from a worker thread. A call that waits for another one shares its
    /// answer.
    pub fn unlock(&self) -> std::result::Result<(), VaultError> {
        let seen = self.unlocks.load(Ordering::SeqCst);
        let _ops = guard(&self.ops);
        if self.unlocks.load(Ordering::SeqCst) != seen {
            return guard(&self.last_unlock).clone();
        }
        let phase = self.inner().phase;
        let answer = match phase {
            Phase::Unlocked => Ok(()),
            Phase::Lost(reason) => Err(VaultError::Lost(reason)),
            Phase::New => self.create(default_protection(self.keystore.caps())),
            Phase::Locked => self.unlock_existing(),
        };
        *guard(&self.last_unlock) = answer.clone();
        self.unlocks.fetch_add(1, Ordering::SeqCst);
        answer
    }

    /// Make the vault: a new data key wrapped by a new wrapping key, and a file with no secret. The `ops` lock is held.
    fn create(&self, protection: Protection) -> std::result::Result<(), VaultError> {
        if protection == Protection::Auth && !self.keystore.caps().has_screen_lock {
            return Err(VaultError::NoScreenLock);
        }
        self.keystore.generate(protection)?;
        let made = self.create_with_key(protection);
        if made.is_err() {
            // Nothing refers to the new wrapping key: do not leave it behind.
            drop(self.keystore.delete());
        }
        made
    }

    fn create_with_key(&self, protection: Protection) -> std::result::Result<(), VaultError> {
        let key: Key = Zeroizing::new(random()?);
        let wrapped = self.keystore.wrap(&*key)?;
        let bytes = seal(protection, &wrapped, &key, &Map::new())?;
        self.files.write(&self.path, &bytes).map_err(Error::Io)?;
        let now = self.clock.now_ms();
        let mut g = self.inner();
        g.key = Some(key);
        g.wrapped = wrapped;
        g.protection = protection;
        g.phase = Phase::Unlocked;
        g.last_now = now;
        Ok(())
    }

    /// Unwrap the data key of the file and check that the body opens. The `ops` lock is held.
    fn unlock_existing(&self) -> std::result::Result<(), VaultError> {
        let Read::Found(parsed) = read_parsed(&*self.files, &self.path) else {
            self.inner().lose(LostReason::Corrupt);
            return Err(VaultError::Lost(LostReason::Corrupt));
        };
        let unwrapped = match self.keystore.unwrap(&parsed.wrapped) {
            Ok(plain) => plain,
            Err(e) => {
                let e = VaultError::from(e);
                if let VaultError::Lost(reason) = e {
                    self.inner().lose(reason);
                }
                return Err(e);
            }
        };
        let mut key: Key = Zeroizing::new([0u8; KEY_LEN]);
        let fits = unwrapped.len() == KEY_LEN;
        if fits {
            key.copy_from_slice(&unwrapped);
        }
        if !fits || parsed.open(&key).is_none() {
            self.inner().lose(LostReason::Corrupt);
            return Err(VaultError::Lost(LostReason::Corrupt));
        }
        let now = self.clock.now_ms();
        let mut g = self.inner();
        g.key = Some(key);
        g.wrapped = parsed.wrapped;
        g.protection = parsed.protection;
        g.phase = Phase::Unlocked;
        g.last_now = now;
        Ok(())
    }

    /// The decrypted secrets. A file that is gone, changed or unreadable makes the vault lost and answers locked.
    fn load(&self, g: &mut Inner) -> Result<Map> {
        let Some(key) = g.key.clone() else { return Err(locked()) };
        let map = match read_parsed(&*self.files, &self.path) {
            Read::Found(parsed) => parsed.open(&key).and_then(|plain| serde_json::from_slice::<Map>(&plain).ok()),
            Read::Missing | Read::Bad => None,
        };
        match map {
            Some(map) if map.len() <= MAX_ENTRIES => Ok(map),
            _ => {
                g.lose(LostReason::Corrupt);
                Err(locked())
            }
        }
    }

    fn store(&self, g: &Inner, map: &Map) -> Result<()> {
        let Some(key) = g.key.as_ref() else { return Err(locked()) };
        let bytes = seal(g.protection, &g.wrapped, key, map)?;
        self.files.write(&self.path, &bytes)?;
        Ok(())
    }

    /// Switch between auth and plain. The data key never changes, only its wrapping. A locked vault is unlocked first
    /// (a prompt for auth); the new file is written before the old wrapping key goes, so a failure in between leaves a
    /// vault that still opens. In the `new` state this creates the vault with that protection. May block: worker thread.
    pub fn set_protection(&self, target: Protection) -> std::result::Result<(), VaultError> {
        let _ops = guard(&self.ops);
        let (phase, current) = {
            let g = self.inner();
            (g.phase, g.protection)
        };
        match phase {
            Phase::Lost(reason) => Err(VaultError::Lost(reason)),
            Phase::New => self.create(target),
            Phase::Locked | Phase::Unlocked if current == target => Ok(()),
            Phase::Locked | Phase::Unlocked => {
                if target == Protection::Auth && !self.keystore.caps().has_screen_lock {
                    return Err(VaultError::NoScreenLock);
                }
                if phase == Phase::Locked {
                    self.unlock_existing()?;
                }
                self.rewrap(target)
            }
        }
    }

    fn rewrap(&self, target: Protection) -> std::result::Result<(), VaultError> {
        let Some(key) = self.inner().key.clone() else { return Err(VaultError::Failed(LOCKED_TEXT.into())) };
        self.keystore.generate(target)?;
        let wrapped = self.keystore.wrap(&*key)?;
        {
            let mut g = self.inner();
            // The secrets are read and the file is written under one lock, so a concurrent `set` is not lost.
            let map = self.load(&mut g)?;
            let bytes = seal(target, &wrapped, &key, &map)?;
            self.files.write(&self.path, &bytes).map_err(Error::Io)?;
            g.protection = target;
            g.wrapped = wrapped;
        }
        // The file names the new key now. A failure here leaves an old key that nothing uses.
        drop(self.keystore.retire_previous());
        Ok(())
    }

    /// Delete the file and the wrapping key and start again with no secret. Only on request, never implicit.
    pub fn reset(&self) -> std::result::Result<(), VaultError> {
        let _ops = guard(&self.ops);
        self.files.remove(&self.path).map_err(Error::Io)?;
        let deleted = self.keystore.delete();
        let mut g = self.inner();
        g.key = None;
        g.wrapped = Vec::new();
        g.hidden_since = None;
        g.phase = Phase::New;
        deleted.map_err(VaultError::from)
    }

    /// The `new` state without a screen lock: a plain vault is made at once, with no prompt.
    fn create_plain_on_demand(&self) -> Result<()> {
        if self.keystore.caps().has_screen_lock {
            return Err(locked());
        }
        let _ops = guard(&self.ops);
        if self.inner().phase == Phase::New {
            self.create(Protection::Plain).map_err(|e| err(&e.to_string()))?;
        }
        Ok(())
    }

    /// The unlocked state under the lock of `inner`, after the clock check, or the locked error.
    fn unlocked(&self) -> Result<MutexGuard<'_, Inner>> {
        let mut g = self.inner();
        self.expire(&mut g, self.clock.now_ms());
        if g.phase == Phase::Unlocked { Ok(g) } else { Err(locked()) }
    }
}

impl SecretStore for VaultStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        check(account, None)?;
        let mut g = self.inner();
        self.expire(&mut g, self.clock.now_ms());
        match g.phase {
            Phase::New => Ok(None),
            Phase::Unlocked => {
                let map = self.load(&mut g)?;
                Ok(map.get(account).map(|s| s.0.clone()))
            }
            Phase::Locked | Phase::Lost(_) => Err(locked()),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        check(account, Some(secret))?;
        if self.state() == VaultState::New {
            self.create_plain_on_demand()?;
        }
        let mut g = self.unlocked()?;
        let mut map = self.load(&mut g)?;
        if !map.contains_key(account) && map.len() >= MAX_ENTRIES {
            return Err(Error::Unsupported(format!("a secret store holds at most {MAX_ENTRIES} secrets")));
        }
        map.insert(account.to_owned(), Secret(secret.to_owned()));
        self.store(&g, &map)
    }

    fn delete(&self, account: &str) -> Result<()> {
        check(account, None)?;
        // Nothing to delete from a vault that does not exist, if no prompt would be needed to make it.
        if self.state() == VaultState::New && !self.keystore.caps().has_screen_lock {
            return Ok(());
        }
        let mut g = self.unlocked()?;
        let mut map = self.load(&mut g)?;
        if map.remove(account).is_none() {
            return Ok(());
        }
        self.store(&g, &map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY: &str = "CANARY-5f3a9c-do-not-leak";

    fn clock() -> Arc<FakeClock> {
        Arc::new(FakeClock::new(1_000))
    }

    /// A vault on a keystore without a screen lock: plain, no prompt.
    fn plain() -> (VaultStore, Arc<FakeKeystore>, Arc<MemoryFiles>) {
        let ks = Arc::new(FakeKeystore::without_screen_lock());
        let files = Arc::new(MemoryFiles::default());
        let store = VaultStore::open_with_files("vault.bin", ks.clone(), clock(), files.clone());
        (store, ks, files)
    }

    fn reopen(ks: &Arc<FakeKeystore>, files: &Arc<MemoryFiles>) -> VaultStore {
        VaultStore::open_with_files("vault.bin", ks.clone(), clock(), files.clone())
    }

    fn is_locked(error: &Error) -> bool {
        matches!(error, Error::Io(_)) && error.to_string() == LOCKED_TEXT
    }

    #[test]
    fn the_locked_text_is_the_one_literal() {
        assert_eq!(LOCKED_TEXT, "secrets are locked");
        assert!(is_locked(&locked()));
    }

    #[test]
    fn a_secret_is_stored_read_replaced_and_removed() {
        let (store, ks, _) = plain();
        assert_eq!(store.state(), VaultState::New);
        assert_eq!(store.get("none").unwrap(), None);
        store.set("ssh:a", "s3cret").unwrap();
        assert_eq!(ks.prompts(), 0, "a plain vault asks nothing");
        assert_eq!(store.state(), VaultState::Unlocked);
        assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("s3cret"));
        assert_eq!(store.get("none").unwrap(), None);
        store.set("ssh:a", "two").unwrap();
        assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("two"));
        store.delete("ssh:a").unwrap();
        assert_eq!(store.get("ssh:a").unwrap(), None);
        store.delete("ssh:a").unwrap();
        store.delete("never-set").unwrap();
    }

    #[test]
    fn a_plain_vault_opens_unlocked_with_its_secrets_after_a_restart() {
        let (store, ks, files) = plain();
        store.set("ssh:a", "one").unwrap();
        drop(store);
        let again = reopen(&ks, &files);
        assert_eq!(again.state(), VaultState::Unlocked);
        assert_eq!(again.get("ssh:a").unwrap().as_deref(), Some("one"));
        assert_eq!(ks.prompts(), 0);
    }

    #[test]
    fn the_file_has_the_format_and_every_flipped_byte_makes_the_vault_lost_never_empty() {
        let (store, ks, files) = plain();
        store.set("ssh:a", "one").unwrap();
        let bytes = files.bytes().unwrap();
        assert_eq!(&bytes[..3], b"HWV");
        assert_eq!(bytes[3], 1, "version");
        assert_eq!(bytes[4], 2, "plain protection");
        let wrapped_len = usize::from(u16::from_be_bytes([bytes[5], bytes[6]]));
        assert_eq!(bytes.len(), 7 + wrapped_len + 24 + b"{\"ssh:a\":\"one\"}".len() + 16);
        for i in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[i] ^= 1;
            files.replace(flipped);
            let opened = reopen(&ks, &files);
            assert_eq!(opened.state(), VaultState::Lost, "byte {i}");
            assert_eq!(opened.lost_reason(), Some(LostReason::Corrupt), "byte {i}");
            assert!(is_locked(&opened.get("ssh:a").unwrap_err()), "byte {i}");
        }
        files.replace(bytes);
        assert_eq!(reopen(&ks, &files).get("ssh:a").unwrap().as_deref(), Some("one"));
    }

    #[test]
    fn truncated_foreign_and_newer_files_are_lost_not_empty() {
        let (store, ks, files) = plain();
        store.set("ssh:a", "one").unwrap();
        let bytes = files.bytes().unwrap();
        for cut in [0, 3, 6, 10, bytes.len() - 1] {
            files.replace(bytes[..cut].to_vec());
            assert_eq!(reopen(&ks, &files).state(), VaultState::Lost, "cut at {cut}");
        }
        let mut newer = bytes.clone();
        newer[3] = 2;
        files.replace(newer);
        assert_eq!(reopen(&ks, &files).state(), VaultState::Lost);
        files.replace(vec![b'x'; 200]);
        assert_eq!(reopen(&ks, &files).state(), VaultState::Lost);
    }

    #[test]
    fn a_secret_is_nowhere_but_in_ciphertext() {
        let (store, ks, files) = plain();
        store.set("ssh:account-name", CANARY).unwrap();
        let bytes = files.bytes().unwrap();
        let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(!has(CANARY), "the secret is in the file");
        assert!(!has("ssh:account-name"), "the account name is in the file");
        assert!(!has("ssh:"), "no part of an account name is in the file");

        let mut shown = vec![format!("{store:?}"), format!("{:?}", store.status()), format!("{ks:?}")];
        let mut errors = vec![
            store.set("", CANARY).unwrap_err().to_string(),
            store.set("a\nb", CANARY).unwrap_err().to_string(),
            store.set("ssh:b", &CANARY.repeat(1000)).unwrap_err().to_string(),
            format!("{:?}", store.set("", CANARY).unwrap_err()),
        ];
        store.lock();
        errors.push(store.get("ssh:account-name").unwrap_err().to_string());
        errors.push(store.set("ssh:account-name", CANARY).unwrap_err().to_string());
        errors.push(store.delete("ssh:account-name").unwrap_err().to_string());
        let (second, _, second_files) = plain();
        second_files.fail_writes(true);
        errors.push(second.set("ssh:a", CANARY).unwrap_err().to_string());
        errors.push(format!("{:?}", second.set("ssh:a", CANARY).unwrap_err()));
        shown.extend(errors);
        for text in &shown {
            assert!(!text.contains(CANARY), "{text}");
            assert!(!text.contains("account-name"), "{text}");
        }
        for e in [KeystoreError::Cancelled, KeystoreError::KeyGone, KeystoreError::NoScreenLock, KeystoreError::Invalid] {
            assert!(!format!("{e} {e:?}").contains(CANARY));
        }
    }

    #[test]
    fn every_write_uses_a_fresh_nonce() {
        let (store, _, files) = plain();
        store.set("a", "same").unwrap();
        let first = files.bytes().unwrap();
        store.set("a", "same").unwrap();
        let second = files.bytes().unwrap();
        assert_ne!(first, second);
        let wrapped_len = usize::from(u16::from_be_bytes([first[5], first[6]]));
        let nonce = 7 + wrapped_len..7 + wrapped_len + 24;
        assert_ne!(first[nonce.clone()], second[nonce]);
    }

    #[test]
    fn a_write_that_fails_leaves_the_old_file_and_the_old_secrets() {
        let (store, ks, files) = plain();
        store.set("a", "old").unwrap();
        let before = files.bytes().unwrap();
        files.fail_writes(true);
        assert!(store.set("a", "new").is_err());
        assert!(store.set("b", "other").is_err());
        assert!(store.delete("a").is_err());
        assert_eq!(files.bytes().unwrap(), before);
        assert_eq!(store.get("a").unwrap().as_deref(), Some("old"));
        files.fail_writes(false);
        assert_eq!(reopen(&ks, &files).get("a").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn a_write_to_the_disk_is_atomic_and_private_and_a_second_open_reads_it() {
        let dir = std::env::temp_dir().join(format!("hw-vault-{}-{}", std::process::id(), 1));
        drop(std::fs::remove_dir_all(&dir));
        let path = dir.join("vault.bin");
        let ks = Arc::new(FakeKeystore::without_screen_lock());
        let store = VaultStore::open(&path, ks.clone(), clock());
        store.set("ssh:a", CANARY).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(!dir.join("vault.bin.tmp").exists());
        drop(store);
        assert_eq!(VaultStore::open(&path, ks, clock()).get("ssh:a").unwrap().as_deref(), Some(CANARY));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_limits_of_a_secret_store_hold() {
        let (store, ..) = plain();
        let memory = crate::secrets::MemoryStore::default();
        let long = "x".repeat(MAX_LEN + 1);
        let cases: [(&str, &str); 4] = [("", "x"), (&long, "x"), ("a\u{7}b", "x"), ("ssh:a", &long)];
        for (account, secret) in cases {
            let mine = store.set(account, secret).unwrap_err();
            let theirs = memory.set(account, secret).unwrap_err();
            assert!(matches!(mine, Error::Unsupported(_)));
            assert_eq!(mine.to_string(), theirs.to_string());
        }
        assert!(store.get("").is_err());
        assert!(store.delete("a\nb").is_err());
        // Exactly the longest values are fine.
        store.set(&"a".repeat(MAX_LEN), &"s".repeat(MAX_LEN)).unwrap();
    }

    #[test]
    fn at_most_4096_accounts_and_a_replacement_at_the_limit_is_allowed() {
        let (store, _, files) = plain();
        store.set("account-0", "s").unwrap();
        {
            // Fill the file to the limit in one write: 4096 single sets would only test the speed of the machine.
            let g = store.inner();
            let key = g.key.as_ref().unwrap();
            let map: Map = (0..MAX_ENTRIES).map(|i| (format!("account-{i}"), Secret("s".into()))).collect();
            files.write(Path::new("vault.bin"), &seal(g.protection, &g.wrapped, key, &map).unwrap()).unwrap();
        }
        let refused = store.set("one-more", "s").unwrap_err();
        assert!(matches!(refused, Error::Unsupported(_)));
        store.set("account-7", "changed").unwrap();
        assert_eq!(store.get("account-7").unwrap().as_deref(), Some("changed"));
        store.delete("account-0").unwrap();
        store.set("one-more", "s").unwrap();
    }

    #[test]
    fn a_file_over_8_mib_is_refused_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("hw-vault-{}-{}", std::process::id(), 2));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        drop(file);
        let store = VaultStore::open(&path, Arc::new(FakeKeystore::without_screen_lock()), clock());
        assert_eq!(store.state(), VaultState::Lost);
        assert_eq!(store.lost_reason(), Some(LostReason::Corrupt));
        // The file is still there: nothing is deleted without a reset.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MAX_FILE + 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_fake_keystore_counts_prompts_cancels_loses_its_key_and_reports_no_lock() {
        let ks = FakeKeystore::new();
        assert_eq!(ks.caps(), KeystoreCaps { has_screen_lock: true, strongbox: false });
        ks.generate(Protection::Auth).unwrap();
        let wrapped = ks.wrap(b"data key").unwrap();
        assert_eq!(ks.prompts(), 1);
        assert_eq!(&**ks.unwrap(&wrapped).as_ref().unwrap(), b"data key");
        assert_eq!(ks.prompts(), 2);
        ks.cancel_next_prompt();
        assert_eq!(ks.unwrap(&wrapped).unwrap_err(), KeystoreError::Cancelled);
        assert_eq!(ks.prompts(), 3);
        assert!(ks.unwrap(&wrapped).is_ok(), "only the next prompt was cancelled");
        let mut changed = wrapped.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(ks.unwrap(&changed).unwrap_err(), KeystoreError::Invalid);
        ks.lose_key();
        assert_eq!(ks.unwrap(&wrapped).unwrap_err(), KeystoreError::KeyGone);
        ks.set_screen_lock(false);
        assert!(!ks.caps().has_screen_lock);
        assert_eq!(ks.generate(Protection::Auth).unwrap_err(), KeystoreError::NoScreenLock);
        ks.generate(Protection::Plain).unwrap();
        let before = ks.prompts();
        let plain = ks.wrap(b"x").unwrap();
        ks.cancel_next_prompt();
        assert!(ks.unwrap(&plain).is_ok(), "a plain key has no prompt to cancel");
        assert_eq!(ks.prompts(), before);
        assert!(!format!("{ks:?}").contains("data key"));
    }

    #[test]
    fn an_old_wrapping_key_stays_until_it_is_retired() {
        let ks = FakeKeystore::new();
        ks.generate(Protection::Plain).unwrap();
        let old = ks.wrap(b"k").unwrap();
        ks.generate(Protection::Plain).unwrap();
        assert!(ks.unwrap(&old).is_ok());
        ks.retire_previous().unwrap();
        assert_eq!(ks.key_count(), 1);
        assert!(ks.unwrap(&old).is_err(), "the retired key is gone");
    }
}
