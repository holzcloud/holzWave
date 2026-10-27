//! Argon2id password verification and the key-encryption key from the same pass.
//!
//! One Argon2id pass over the password and a 16-byte random salt yields 64 bytes. The first 32 are the verifier that the
//! database holds; the last 32 are the key-encryption key that unlocks the person's secret store and is never stored.
//! Verification derives both again, compares the verifier in constant time and hands the key-encryption key to the caller
//! in a [`Kek`], which wipes itself and prints a marker.
//!
//! Argon2 is a lever: every derivation takes 64 MiB and about a second of one core, and anyone can ask for one. So the work
//! goes through an [`ArgonGate`] that lets a few derivations run at once, lets a bounded number of callers wait and turns
//! everyone beyond that away at once with [`PasswordError::Busy`]. The input is checked before a derivation is started, and
//! a name that has no usable password costs the same derivation as a wrong password.

use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use subtle::{Choice, ConstantTimeEq};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use zeroize::Zeroizing;

use crate::limits::ARGON2_CONCURRENCY;
use crate::store::{AccountId, AccountState, PasswordCredential, Store, StoreError, Tx};
use crate::unlock::{Kdf, Kek, SALT_BYTES, UnlockError, argon2id_into, random};

/// The length of the verifier in bytes: the first half of the derivation.
pub const VERIFIER_BYTES: usize = 32;

/// The length of the whole derivation in bytes.
const DERIVED_BYTES: usize = 64;

/// The fewest characters of a password.
pub const MIN_PASSWORD_CHARS: usize = 12;

/// The most bytes of a password.
pub const MAX_PASSWORD_BYTES: usize = 256;

/// The most callers that wait for a derivation. The one after them is turned away.
pub const MAX_WAITING: usize = 32;

/// The only algorithm name a stored parameters text may carry.
const ALGORITHM: &str = "argon2id";

/// The only Argon2 version a stored parameters text may carry (0x13).
const VERSION: u32 = 19;

/// What the `Debug` text of a derivation prints.
const DERIVED_DEBUG: &str = "Derived(..)";

/// What the `Debug` text of a prepared credential prints.
const PREPARED_DEBUG: &str = "Prepared(..)";

/// Which rule a password broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyViolation {
    /// Fewer than 12 characters.
    TooShort,
    /// More than 256 bytes.
    TooLong,
    /// The same as the username, whatever the case.
    SameAsUsername,
    /// One character, repeated.
    Repeated,
}

impl PolicyViolation {
    /// The name of the rule.
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyViolation::TooShort => "too_short",
            PolicyViolation::TooLong => "too_long",
            PolicyViolation::SameAsUsername => "same_as_username",
            PolicyViolation::Repeated => "repeated",
        }
    }
}

/// Why a password was refused or could not be checked. No text holds the password, a verifier or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    /// The name or the password is not right. One error for an unknown account, a missing password, a disabled account and a
    /// wrong password, so that the outcome tells nothing about which case applied.
    InvalidCredentials,
    /// A new password broke a rule of the policy. No derivation was made.
    Policy(PolicyViolation),
    /// Too many derivations are running and waiting. Nothing was queued.
    Busy,
    /// The stored row cannot be used: a parameter outside the bounds, a text that does not parse, a wrong length. No
    /// derivation was made.
    CorruptCredential,
    /// The operating system or the derivation failed.
    Derive,
    /// The store failed.
    Store(StoreError),
}

impl fmt::Display for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasswordError::InvalidCredentials => f.write_str("the name or the password is not right"),
            PasswordError::Policy(rule) => write!(f, "the password breaks a rule ({})", rule.as_str()),
            PasswordError::Busy => f.write_str("too many sign-ins are being checked, try again in a moment"),
            PasswordError::CorruptCredential => f.write_str("the stored password credential cannot be used"),
            PasswordError::Derive => f.write_str("the password could not be processed"),
            PasswordError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PasswordError {}

impl From<StoreError> for PasswordError {
    fn from(e: StoreError) -> PasswordError {
        PasswordError::Store(e)
    }
}

impl From<UnlockError> for PasswordError {
    fn from(_: UnlockError) -> PasswordError {
        PasswordError::Derive
    }
}

/// The two halves of one derivation. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct Derived {
    verifier: Zeroizing<[u8; VERIFIER_BYTES]>,
    kek: Kek,
}

impl Derived {
    /// The verifier, the half the database holds.
    pub fn verifier(&self) -> &[u8; VERIFIER_BYTES] {
        &self.verifier
    }

    /// The key-encryption key, the half that is never stored.
    pub fn kek(&self) -> &Kek {
        &self.kek
    }

    /// Gives up the verifier (wiped here) and keeps the key-encryption key.
    pub fn into_kek(self) -> Kek {
        self.kek
    }
}

impl fmt::Debug for Derived {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DERIVED_DEBUG)
    }
}

/// One Argon2id pass over `password` and `salt` split into the verifier (bytes 0 to 31) and the key-encryption key (bytes 32
/// to 63). The password is used byte for byte as typed. This runs on the calling thread and takes about a second; the sign-in
/// code goes through [`ArgonGate`] instead.
pub fn derive_split(password: &[u8], salt: &[u8; SALT_BYTES], kdf: &Kdf) -> Result<Derived, PasswordError> {
    let mut out = Zeroizing::new([0u8; DERIVED_BYTES]);
    argon2id_into(password, salt, kdf, &mut out[..])?;
    let mut verifier = Zeroizing::new([0u8; VERIFIER_BYTES]);
    verifier.copy_from_slice(&out[..VERIFIER_BYTES]);
    let mut key = Zeroizing::new([0u8; VERIFIER_BYTES]);
    key.copy_from_slice(&out[VERIFIER_BYTES..]);
    Ok(Derived { verifier, kek: Kek::from_bytes(&key) })
}

/// A new random salt.
pub fn new_salt() -> Result<[u8; SALT_BYTES], PasswordError> {
    Ok(random::<SALT_BYTES>()?)
}

// ---- the parameters text ----

/// The parameters as the text the row keeps.
pub fn params_text(kdf: &Kdf) -> String {
    format!(r#"{{"alg":"{ALGORITHM}","v":{VERSION},"m":{},"t":{},"p":{}}}"#, kdf.m_kib(), kdf.t(), kdf.p())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredParams {
    alg: String,
    v: u32,
    m: u32,
    t: u32,
    p: u32,
}

/// The parameters of a stored text, or [`PasswordError::CorruptCredential`]. A missing or unknown member, another algorithm
/// or version, and a value outside the bounds of [`Kdf`] are all refused here, before any derivation, so a tampered row
/// cannot ask for a free derivation or for gigabytes.
pub fn parse_params(text: &str) -> Result<Kdf, PasswordError> {
    let stored: StoredParams = serde_json::from_str(text).map_err(|_| PasswordError::CorruptCredential)?;
    if stored.alg != ALGORITHM || stored.v != VERSION {
        return Err(PasswordError::CorruptCredential);
    }
    Kdf::new(stored.m, stored.t, stored.p).map_err(|_| PasswordError::CorruptCredential)
}

// ---- the policy ----

/// Checks a new password against the rules: 12 to 256 bytes and at least 12 characters, not the username (whatever the
/// case), not one character repeated. The password is looked at as typed, with no Unicode normalisation. No derivation is
/// involved, so a refusal costs nothing.
pub fn check_policy(username: &str, password: &str) -> Result<(), PolicyViolation> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PolicyViolation::TooLong);
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PolicyViolation::TooShort);
    }
    if password.to_lowercase() == username.to_lowercase() {
        return Err(PolicyViolation::SameAsUsername);
    }
    let mut chars = password.chars();
    if let Some(first) = chars.next()
        && chars.all(|c| c == first)
    {
        return Err(PolicyViolation::Repeated);
    }
    Ok(())
}

// ---- the gate ----

/// Called on the derivation's thread when a derivation starts, before the Argon2 work. For tests that count or hold
/// derivations.
pub type Hook = Arc<dyn Fn() + Send + Sync>;

struct GateInner {
    permits: Arc<Semaphore>,
    size: usize,
    waiting: AtomicUsize,
    max_waiting: usize,
    kdf: Kdf,
    derivations: AtomicU64,
}

/// The bounded door to Argon2. At most `permits` derivations run at once, on threads of their own, never on the async
/// threads; at most [`MAX_WAITING`] callers wait for one; the next caller gets [`PasswordError::Busy`] at once. A caller that
/// is dropped while it waits gives its place back. A permit is held until the derivation itself has finished, so dropping the
/// caller during a derivation does not let a further derivation start beside it.
#[derive(Clone)]
pub struct ArgonGate {
    inner: Arc<GateInner>,
    hook: Option<Hook>,
}

impl fmt::Debug for ArgonGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArgonGate(..)")
    }
}

impl ArgonGate {
    /// A gate with `permits` concurrent derivations (at least one) that makes new credentials with `kdf`. The dummy derivation
    /// of an unknown name uses the same parameters.
    pub fn new(permits: u32, kdf: Kdf) -> ArgonGate {
        let size = usize::try_from(permits.max(1)).unwrap_or(1);
        ArgonGate {
            inner: Arc::new(GateInner {
                permits: Arc::new(Semaphore::new(size)),
                size,
                waiting: AtomicUsize::new(0),
                max_waiting: MAX_WAITING,
                kdf,
                derivations: AtomicU64::new(0),
            }),
            hook: None,
        }
    }

    /// The gate of the server: the default concurrency and the default parameters.
    pub fn standard() -> ArgonGate {
        ArgonGate::new(ARGON2_CONCURRENCY, Kdf::default())
    }

    /// The same gate with a hook that runs at the start of every derivation.
    pub fn with_hook(mut self, hook: Hook) -> ArgonGate {
        self.hook = Some(hook);
        self
    }

    /// The parameters new credentials are made with.
    pub fn kdf(&self) -> Kdf {
        self.inner.kdf
    }

    /// How many derivations have started on this gate (and its clones), the dummy ones included.
    pub fn derivations(&self) -> u64 {
        self.inner.derivations.load(Ordering::SeqCst)
    }

    /// How many callers wait for a permit now.
    pub fn waiting(&self) -> usize {
        self.inner.waiting.load(Ordering::SeqCst)
    }

    /// How many permits are free now.
    pub fn available(&self) -> usize {
        self.inner.permits.available_permits()
    }

    /// How many derivations may run at once.
    pub fn permits(&self) -> usize {
        self.inner.size
    }

    /// How many callers may wait.
    pub fn max_waiting(&self) -> usize {
        self.inner.max_waiting
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, PasswordError> {
        match self.inner.permits.clone().try_acquire_owned() {
            Ok(permit) => return Ok(permit),
            Err(TryAcquireError::NoPermits) => {}
            Err(TryAcquireError::Closed) => return Err(PasswordError::Derive),
        }
        let mut seen = self.inner.waiting.load(Ordering::SeqCst);
        loop {
            if seen >= self.inner.max_waiting {
                return Err(PasswordError::Busy);
            }
            match self.inner.waiting.compare_exchange(seen, seen + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(now) => seen = now,
            }
        }
        let _place = WaitingPlace(&self.inner.waiting);
        self.inner.permits.clone().acquire_owned().await.map_err(|_| PasswordError::Derive)
    }

    /// One derivation through the gate, on a blocking thread.
    pub async fn derive(&self, password: &str, salt: [u8; SALT_BYTES], kdf: Kdf) -> Result<Derived, PasswordError> {
        let permit = self.acquire().await?;
        let bytes = Zeroizing::new(password.as_bytes().to_vec());
        let inner = self.inner.clone();
        let hook = self.hook.clone();
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            inner.derivations.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = &hook {
                hook();
            }
            derive_split(&bytes, &salt, &kdf)
        });
        work.await.map_err(|_| PasswordError::Derive)?
    }
}

/// A place in the queue; it is given back when the waiting caller finishes or is dropped.
struct WaitingPlace<'a>(&'a AtomicUsize);

impl Drop for WaitingPlace<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---- enrolment ----

/// A credential that is derived but not yet stored: made outside any transaction by [`prepare_password`] and written by
/// [`store_prepared`] in the caller's transaction. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct Prepared {
    derived: Derived,
    salt: [u8; SALT_BYTES],
    params: String,
}

impl Prepared {
    /// The parameters text the row will hold.
    pub fn params(&self) -> &str {
        &self.params
    }

    /// The salt the row will hold.
    pub fn salt(&self) -> &[u8; SALT_BYTES] {
        &self.salt
    }

    /// The key-encryption key of the new password.
    pub fn kek(&self) -> &Kek {
        self.derived.kek()
    }

    /// Gives up the rest and keeps the key-encryption key.
    pub fn into_kek(self) -> Kek {
        self.derived.into_kek()
    }
}

impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREPARED_DEBUG)
    }
}

/// Checks the policy, then makes one derivation through the gate with a new random salt. Call it outside any transaction: it
/// can take a second, and it holds nothing of the store.
pub async fn prepare_password(gate: &ArgonGate, username: &str, password: &str) -> Result<Prepared, PasswordError> {
    check_policy(username, password).map_err(PasswordError::Policy)?;
    let salt = new_salt()?;
    let kdf = gate.kdf();
    let derived = gate.derive(password, salt, kdf).await?;
    Ok(Prepared { derived, salt, params: params_text(&kdf) })
}

/// Writes a prepared credential for `account` in the caller's transaction, replacing an earlier one. Nothing is derived.
pub fn store_prepared(tx: &Tx<'_>, account: AccountId, prepared: &Prepared, now: i64) -> Result<(), PasswordError> {
    tx.password_set(
        account,
        &PasswordCredential {
            verifier: prepared.derived.verifier().to_vec(),
            salt: prepared.salt.to_vec(),
            params: prepared.params.clone(),
            changed_at: now,
        },
    )?;
    Ok(())
}

/// Sets the password of `account`: [`prepare_password`] and then [`store_prepared`] in one transaction. Answers the
/// key-encryption key of the new password.
pub async fn enrol_password(
    gate: &ArgonGate,
    store: &Store,
    account: AccountId,
    password: &str,
    now: i64,
) -> Result<Kek, PasswordError> {
    let username = store.account_get(account)?.ok_or(PasswordError::Store(StoreError::NotFound))?.username;
    let prepared = prepare_password(gate, &username, password).await?;
    store.write(|tx| store_prepared(tx, account, &prepared, now).map_err(into_store_error))?;
    Ok(prepared.into_kek())
}

fn into_store_error(e: PasswordError) -> StoreError {
    match e {
        PasswordError::Store(e) => e,
        _ => StoreError::Invalid("the password credential could not be stored"),
    }
}

// ---- sign-in ----

/// What the store knows about the name a person typed: the account, its state and its password row. `None` in place of this
/// stands for every other case (no such name, no password set), and [`verify_password`] treats those the same.
#[derive(Clone)]
pub struct StoredPassword {
    account: AccountId,
    state: AccountState,
    credential: PasswordCredential,
}

impl StoredPassword {
    /// The pieces of a lookup.
    pub fn new(account: AccountId, state: AccountState, credential: PasswordCredential) -> StoredPassword {
        StoredPassword { account, state, credential }
    }

    /// The account.
    pub fn account(&self) -> AccountId {
        self.account
    }
}

impl fmt::Debug for StoredPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredPassword").field("account", &self.account).field("state", &self.state).finish_non_exhaustive()
    }
}

/// The account called `username` and its password row. `None` when there is no such account (or it is deleted) or it has no
/// password. A disabled account is returned, so that its check costs the derivation of any other.
pub fn lookup_password(tx: &Tx<'_>, username: &str) -> Result<Option<StoredPassword>, StoreError> {
    if !crate::store::valid_username(username) {
        return Ok(None);
    }
    let Some(account) = tx.account_by_username(username)? else {
        return Ok(None);
    };
    if account.state == AccountState::Deleted {
        return Ok(None);
    }
    Ok(tx.password_get(account.id)?.map(|credential| StoredPassword::new(account.id, account.state, credential)))
}

/// A password that matched an active account.
#[derive(Debug)]
pub struct Verified {
    /// The account.
    pub account: AccountId,
    /// The key-encryption key of the password, for the caller to hand to the worker.
    pub kek: Kek,
}

/// The salt of the dummy derivation, made once per process.
fn dummy_salt() -> [u8; SALT_BYTES] {
    static SALT: OnceLock<[u8; SALT_BYTES]> = OnceLock::new();
    *SALT.get_or_init(|| random::<SALT_BYTES>().unwrap_or([0x5a; SALT_BYTES]))
}

/// Checks `password` against what [`lookup_password`] found. Whatever the case (no account, no password, a disabled account, a
/// wrong password) exactly one derivation runs and the answer is the one [`PasswordError::InvalidCredentials`]. An empty
/// password and one longer than the limit are turned away before any derivation. A stored row that cannot be used is
/// [`PasswordError::CorruptCredential`], also before any derivation.
pub async fn verify_password(
    gate: &ArgonGate,
    stored: Option<StoredPassword>,
    password: &str,
) -> Result<Verified, PasswordError> {
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::InvalidCredentials);
    }
    let Some(stored) = stored else {
        // Nothing to compare with: do the work of a real check and answer the same.
        gate.derive(password, dummy_salt(), gate.kdf()).await?;
        return Err(PasswordError::InvalidCredentials);
    };
    let kdf = parse_params(&stored.credential.params)?;
    let salt: [u8; SALT_BYTES] =
        stored.credential.salt.as_slice().try_into().map_err(|_| PasswordError::CorruptCredential)?;
    if stored.credential.verifier.len() != VERIFIER_BYTES {
        return Err(PasswordError::CorruptCredential);
    }
    let derived = gate.derive(password, salt, kdf).await?;
    let same = derived.verifier().ct_eq(&stored.credential.verifier[..]);
    let active = Choice::from(u8::from(stored.state == AccountState::Active));
    if bool::from(same & active) {
        Ok(Verified { account: stored.account, kek: derived.into_kek() })
    } else {
        Err(PasswordError::InvalidCredentials)
    }
}
