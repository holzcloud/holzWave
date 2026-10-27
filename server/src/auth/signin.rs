//! The password sign-in in two steps, and the session it ends in.
//!
//! [`SignIn::begin_password`] checks a user name and a password and, when they are right, answers a [`PendingToken`]: a random
//! value for the half-signed-in state and nothing more. No session exists after it. [`SignIn::finish_totp`] takes the token
//! and a six-digit code, checks the code once (the replay guard of the store) and only then makes the session. The pending
//! state lasts a few minutes, allows a few code attempts, is removed by success, by the last failed attempt and by expiry, and
//! at most [`MAX_PENDING`] of them exist at once.
//!
//! Every refusal of the first step is the one [`SignInError::Invalid`]: an unknown name, a disabled account, an account
//! without a password or without an authenticator and a wrong password all run the same derivation and end the same way.
//! Nothing here holds a password, a code, a token or a key in an error, a `Debug` text, a log line or an audit line. The audit
//! line of an event is built from the closed vocabulary of the audit module, so a password typed into the name field cannot
//! reach the file.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use zeroize::Zeroizing;

use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::password::{ArgonGate, MAX_PASSWORD_BYTES, PasswordError, lookup_password, verify_password};
use crate::auth::session::{Created, Method, SessionManager, parse_token};
use crate::auth::totp::{self, TotpError};
use crate::clock::Clock;
use crate::limits::Limits;
use crate::log::{self, Level};
use crate::sealed::Sealer;
use crate::store::{AccountId, AccountState, Store, StoreError, TokenHash};
use crate::unlock::{Kek, random};

/// The most half-signed-in states that exist at once. The one after them is [`SignInError::Busy`].
pub const MAX_PENDING: usize = 4_096;

/// How many wrong codes one pending state allows. The last one removes it.
pub const CODE_ATTEMPTS: u8 = 3;

/// The longest user name that is looked at, in bytes.
pub const MAX_USERNAME_BYTES: usize = 128;

/// The method of every session this module makes.
const PASSWORD_METHOD: Method = Method::PasswordTotp;

/// What the `Debug` text of a pending token prints.
const PENDING_DEBUG: &str = "PendingToken(..)";

/// What the `Debug` text of the unlock material prints.
const MATERIAL_DEBUG: &str = "UnlockMaterial(..)";

/// Why a sign-in step was refused. No text holds a name, a password, a code, a token or a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInError {
    /// The name, the password, the code or the token is not right. One error for every reason, so that the outcome tells
    /// nothing about which part was wrong.
    Invalid,
    /// Too many tries; the lock ends in `retry_seconds` (at least one).
    Throttled {
        /// Whole seconds until the lock ends.
        retry_seconds: u64,
    },
    /// Too many sign-ins are being processed. Nothing was queued.
    Busy,
    /// Something failed that the person cannot fix. The text says no more than that.
    Internal,
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignInError::Invalid => f.write_str("the name, the password or the code is not right"),
            SignInError::Throttled { retry_seconds } => {
                write!(f, "too many tries, try again in {retry_seconds} seconds")
            }
            SignInError::Busy => f.write_str("too many sign-ins are being checked, try again in a moment"),
            SignInError::Internal => f.write_str("the sign-in could not be completed"),
        }
    }
}

impl std::error::Error for SignInError {}

impl From<StoreError> for SignInError {
    fn from(_: StoreError) -> SignInError {
        SignInError::Internal
    }
}

/// The value of the half-signed-in state: 32 random bytes as 43 base64url characters. Not `Clone`, a marker for `Debug`,
/// wiped when dropped. It opens nothing but the second step.
pub struct PendingToken(Zeroizing<String>);

impl PendingToken {
    /// The 43 characters, for the cookie of the second step.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PendingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PENDING_DEBUG)
    }
}

/// The key-encryption key of the password, carried from the first step to the second and handed to the caller with the
/// session, so that the person's secret store can be opened at sign-in. Not `Clone`, no serialization, a marker for `Debug`,
/// wiped when dropped.
pub struct UnlockMaterial {
    kek: Kek,
    wiped: Arc<AtomicU64>,
}

impl UnlockMaterial {
    /// The key.
    pub fn kek(&self) -> &Kek {
        &self.kek
    }
}

impl fmt::Debug for UnlockMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MATERIAL_DEBUG)
    }
}

impl Drop for UnlockMaterial {
    fn drop(&mut self) {
        // The key itself is wiped by its own type when the field drops right after this; the count is how a test sees that
        // every end of a pending state let go of its material.
        self.wiped.fetch_add(1, Ordering::SeqCst);
    }
}

/// What a finished sign-in hands to the caller.
#[derive(Debug)]
pub struct Grant {
    /// The new session and the value for its cookie.
    pub session: Created,
    /// The key for the person's secret store.
    pub unlock: UnlockMaterial,
}

/// Which step of a flow an audit line is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Password,
    Totp,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Step::Password => "password",
            Step::Totp => "totp",
        }
    }
}

/// One half-signed-in state.
struct Entry {
    account: AccountId,
    created: i64,
    attempts_left: u8,
    in_flight: bool,
    material: UnlockMaterial,
}

/// The sign-in service.
pub struct SignIn {
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    audit: Arc<Audit>,
    sessions: Arc<SessionManager>,
    gate: ArgonGate,
    sealer: Arc<Sealer>,
    limits: Limits,
    pending: Mutex<HashMap<TokenHash, Entry>>,
    wiped: Arc<AtomicU64>,
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SignIn(..)")
    }
}

impl SignIn {
    /// The service over the pieces the earlier parts built, with the default limits.
    pub fn new(
        store: Arc<Store>,
        clock: Arc<dyn Clock>,
        audit: Arc<Audit>,
        sessions: Arc<SessionManager>,
        gate: ArgonGate,
        sealer: Arc<Sealer>,
    ) -> SignIn {
        SignIn {
            store,
            clock,
            audit,
            sessions,
            gate,
            sealer,
            limits: Limits::DEFAULT,
            pending: Mutex::new(HashMap::new()),
            wiped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The same service with the limits of the configuration.
    pub fn with_limits(mut self, limits: Limits) -> SignIn {
        self.limits = limits;
        self
    }

    /// How many half-signed-in states exist now.
    pub fn pending_count(&self) -> usize {
        self.lock().len()
    }

    /// How many pieces of unlock material have been let go of so far (each wipes its key as it goes). For tests and metrics.
    pub fn wiped_count(&self) -> u64 {
        self.wiped.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TokenHash, Entry>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn pending_secs(&self) -> i64 {
        i64::try_from(self.limits.pending_signin.as_secs()).unwrap_or(i64::MAX)
    }

    // ---- the audit ----

    fn record(&self, event: &Event) {
        if self.audit.record(event).is_err() {
            log::event(Level::Error, "signin.audit_failed", &[]);
        }
    }

    /// The one line of a refused step, and the error to answer.
    fn refuse(&self, step: Step, target: Option<AccountId>, addr: IpAddr, error: SignInError) -> SignInError {
        let reason = if error == SignInError::Busy { "busy" } else { "invalid" };
        let mut event = Event::new(Action::SigninFail, Outcome::Fail, Actor::Anonymous, Some(addr))
            .detail(DetailKey::Step, DetailValue::Text(step.as_str()))
            .detail(DetailKey::Reason, DetailValue::Text(reason));
        if let Some(target) = target {
            event = event.target(target);
        }
        self.record(&event);
        error
    }

    // ---- the pending states ----

    /// Removes the states past their time, except those a second step is working on.
    fn sweep(map: &mut HashMap<TokenHash, Entry>, now: i64, secs: i64) {
        map.retain(|_, e| e.in_flight || now <= e.created.saturating_add(secs));
    }

    /// Whether a new state fits now.
    fn has_room(&self, now: i64) -> bool {
        let mut map = self.lock();
        if map.len() >= MAX_PENDING {
            Self::sweep(&mut map, now, self.pending_secs());
        }
        map.len() < MAX_PENDING
    }

    /// Takes the state of a token for a code check: it must exist, be within its time and not be checked right now.
    fn reserve(&self, hash: &TokenHash, now: i64) -> Option<AccountId> {
        let mut map = self.lock();
        let expired = map.get(hash).is_some_and(|e| !e.in_flight && now > e.created.saturating_add(self.pending_secs()));
        if expired {
            map.remove(hash);
            return None;
        }
        let entry = map.get_mut(hash)?;
        if entry.in_flight {
            return None;
        }
        entry.in_flight = true;
        Some(entry.account)
    }

    /// Gives a state back after a check that did not count (a failure of the store).
    fn release(&self, hash: &TokenHash) {
        if let Some(entry) = self.lock().get_mut(hash) {
            entry.in_flight = false;
        }
    }

    /// Counts a wrong code against a state and removes it when none is left.
    fn spend_attempt(&self, hash: &TokenHash) {
        let mut map = self.lock();
        let Some(entry) = map.get_mut(hash) else { return };
        entry.in_flight = false;
        entry.attempts_left = entry.attempts_left.saturating_sub(1);
        if entry.attempts_left == 0 {
            map.remove(hash);
        }
    }

    // ---- the first step ----

    /// The first step: checks the user name and the password and, when they are right, answers the token of the
    /// half-signed-in state. Nothing is signed in. Whatever is wrong, the answer is [`SignInError::Invalid`] after exactly one
    /// derivation; input of a wrong size is turned away before it.
    pub async fn begin_password(
        &self,
        username: &str,
        password: &str,
        addr: IpAddr,
    ) -> Result<PendingToken, SignInError> {
        let now = self.clock.unix();
        if username.is_empty()
            || username.len() > MAX_USERNAME_BYTES
            || password.is_empty()
            || password.len() > MAX_PASSWORD_BYTES
        {
            return Err(self.refuse(Step::Password, None, addr, SignInError::Invalid));
        }
        if !self.has_room(now) {
            return Err(self.refuse(Step::Password, None, addr, SignInError::Busy));
        }
        let folded = username.to_lowercase();
        let (known, stored) = self.store.read(|tx| {
            let account = tx.account_by_username(&folded)?.filter(|a| a.state != AccountState::Deleted);
            let Some(account) = account else { return Ok((None, None)) };
            // An account that is disabled, has no password or has no authenticator is checked like one that does not exist.
            let usable = account.state == AccountState::Active && tx.totp_get(account.id)?.is_some();
            let stored = if usable { lookup_password(tx, &folded)? } else { None };
            Ok((Some(account.id), stored))
        })?;
        match verify_password(&self.gate, stored, password).await {
            Ok(verified) => {
                let (token, hash) = new_token().map_err(|_| SignInError::Internal)?;
                let entry = Entry {
                    account: verified.account,
                    created: now,
                    attempts_left: CODE_ATTEMPTS,
                    in_flight: false,
                    material: UnlockMaterial { kek: verified.kek, wiped: self.wiped.clone() },
                };
                let mut map = self.lock();
                if map.len() >= MAX_PENDING {
                    Self::sweep(&mut map, now, self.pending_secs());
                }
                if map.len() >= MAX_PENDING {
                    drop(map);
                    return Err(self.refuse(Step::Password, None, addr, SignInError::Busy));
                }
                map.insert(hash, entry);
                Ok(token)
            }
            Err(PasswordError::InvalidCredentials) => Err(self.refuse(Step::Password, known, addr, SignInError::Invalid)),
            Err(PasswordError::Busy) => Err(self.refuse(Step::Password, None, addr, SignInError::Busy)),
            Err(_) => Err(SignInError::Internal),
        }
    }

    // ---- the second step ----

    /// The second step: checks the six-digit code of the account behind `token` and, only if it is right and unused, makes the
    /// session (method password and code) and ends the half-signed-in state. The state is gone after every outcome that
    /// counts: success, the last wrong code and expiry. `device` is a short summary of the browser.
    pub fn finish_totp(&self, token: &str, code: &str, addr: IpAddr, device: &str) -> Result<Grant, SignInError> {
        let now = self.clock.unix();
        let Ok(hash) = parse_token(token) else {
            return Err(self.refuse(Step::Totp, None, addr, SignInError::Invalid));
        };
        let Some(account) = self.reserve(&hash, now) else {
            return Err(self.refuse(Step::Totp, None, addr, SignInError::Invalid));
        };
        let checked = self.store.write(|tx| Ok(totp::verify_and_consume(tx, &self.sealer, account, code, now)));
        match checked {
            Ok(Ok(())) => {}
            Ok(Err(TotpError::InvalidCode)) => {
                self.spend_attempt(&hash);
                return Err(self.refuse(Step::Totp, Some(account), addr, SignInError::Invalid));
            }
            _ => {
                self.release(&hash);
                return Err(SignInError::Internal);
            }
        }
        let Some(entry) = self.lock().remove(&hash) else {
            return Err(SignInError::Internal);
        };
        match self.sessions.create(account, PASSWORD_METHOD, now, device, Some(addr)) {
            Ok(session) => {
                self.record(
                    &Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(account), Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(PASSWORD_METHOD.as_str())),
                );
                Ok(Grant { session, unlock: entry.material })
            }
            Err(_) => Err(self.refuse(Step::Totp, Some(account), addr, SignInError::Invalid)),
        }
    }
}

/// A new pending token and the hash it is kept under.
fn new_token() -> Result<(PendingToken, TokenHash), SignInError> {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let bytes = Zeroizing::new(random::<32>().map_err(|_| SignInError::Internal)?);
    let value = PendingToken(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])));
    Ok((value, crate::store::hash_token(&bytes[..])))
}
