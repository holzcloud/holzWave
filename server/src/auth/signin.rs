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
use crate::auth::throttle::{Policy, Throttle, account_key, address_key};
use crate::sealed::Sealer;
use crate::store::{AccountId, AccountState, Role, Session, Store, StoreError, TokenHash};
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
enum Stage {
    Password,
    Totp,
    StepUp,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::Password => "password",
            Stage::Totp => "totp",
            Stage::StepUp => "step_up",
        }
    }
}

/// Who a step is counted against.
#[derive(Clone, Copy)]
struct Who<'a> {
    /// The key of the typed name, when there is one.
    account: Option<&'a str>,
    /// The key of the client address.
    address: &'a str,
    /// The client address, for the line.
    addr: IpAddr,
}

/// One half-signed-in state.
struct Entry {
    account: AccountId,
    created: i64,
    attempts_left: u8,
    in_flight: bool,
    /// The key the typed name is counted under; the second step counts and clears it.
    account_key: String,
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
    throttle: Throttle,
    max_pending: usize,
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
        let limits = Limits::DEFAULT;
        SignIn {
            throttle: Throttle::new(store.clone(), Policy::from_limits(&limits)),
            store,
            clock,
            audit,
            sessions,
            gate,
            sealer,
            limits,
            max_pending: MAX_PENDING,
            pending: Mutex::new(HashMap::new()),
            wiped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The same service with the limits of the configuration.
    pub fn with_limits(mut self, limits: Limits) -> SignIn {
        self.throttle = Throttle::new(self.store.clone(), Policy::from_limits(&limits));
        self.limits = limits;
        self
    }

    /// The same service with room for fewer half-signed-in states than [`MAX_PENDING`] (never more). For tests that fill the
    /// room, because every state costs a real derivation.
    pub fn with_max_pending(mut self, most: usize) -> SignIn {
        self.max_pending = most.clamp(1, MAX_PENDING);
        self
    }

    /// The most half-signed-in states this service keeps at once.
    pub fn max_pending(&self) -> usize {
        self.max_pending
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

    /// Counts a failed step and writes its one line, then answers `error`. `account` is the key of the typed name, or `None`
    /// for a failure that is not about a name (a token that was never issued, a server too busy): that one counts under the
    /// address alone, so that nobody can lock an account by keeping the server busy. The step that reaches a limit writes
    /// `signin.locked` instead of `signin.fail`.
    fn fail(&self, step: Stage, target: Option<AccountId>, who: Who<'_>, now: i64, error: SignInError) -> SignInError {
        let account = if error == SignInError::Busy { None } else { who.account };
        let locked = match self.throttle.record_failure(account, who.address, now) {
            Ok(counted) => counted.started_lock,
            Err(_) => {
                log::event(Level::Error, "signin.throttle_failed", &[]);
                false
            }
        };
        let reason = if error == SignInError::Busy { "busy" } else { "invalid" };
        let action = if locked { Action::SigninLocked } else { Action::SigninFail };
        // A step-up is made by someone who is signed in; the other steps are made by nobody yet.
        let actor = match (step, target) {
            (Stage::StepUp, Some(account)) => Actor::Account(account),
            _ => Actor::Anonymous,
        };
        let mut event = Event::new(action, Outcome::Fail, actor, Some(who.addr))
            .detail(DetailKey::Step, DetailValue::Text(step.as_str()));
        if !locked {
            event = event.detail(DetailKey::Reason, DetailValue::Text(reason));
        }
        if let Some(target) = target {
            event = event.target(target);
        }
        self.record(&event);
        error
    }

    /// The error of an attempt made during a lock, or `None` when none applies. Nothing is counted and no line is written, so
    /// a flood of refused attempts grows nothing.
    fn locked(&self, account: Option<&str>, address: &str, now: i64) -> Result<(), SignInError> {
        match self.throttle.check(account, address, now) {
            Ok(None) => Ok(()),
            Ok(Some(retry_seconds)) => Err(SignInError::Throttled { retry_seconds }),
            Err(_) => Err(SignInError::Internal),
        }
    }

    // ---- the pending states ----

    /// Removes the states past their time, except those a second step is working on.
    fn sweep(map: &mut HashMap<TokenHash, Entry>, now: i64, secs: i64) {
        map.retain(|_, e| e.in_flight || now <= e.created.saturating_add(secs));
    }

    /// Whether a new state fits now.
    fn has_room(&self, now: i64) -> bool {
        let mut map = self.lock();
        if map.len() >= self.max_pending {
            Self::sweep(&mut map, now, self.pending_secs());
        }
        map.len() < self.max_pending
    }

    /// Takes the state of a token for a code check: it must exist, be within its time and not be checked right now.
    fn reserve(&self, hash: &TokenHash, now: i64) -> Option<(AccountId, String)> {
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
        Some((entry.account, entry.account_key.clone()))
    }

    /// Ends a state for good, wiping its material.
    fn discard(&self, hash: &TokenHash) {
        self.lock().remove(hash);
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
    /// half-signed-in state. Nothing is signed in.
    ///
    /// In order, each step before anything that costs more: the lock of the name and of the address ([`SignInError::Throttled`],
    /// no line, no derivation), the size of the input, room for one more pending state ([`SignInError::Busy`]), and then one
    /// derivation. Whatever is wrong after that, the answer is [`SignInError::Invalid`] and a failure is counted under the name
    /// and the address.
    pub async fn begin_password(
        &self,
        username: &str,
        password: &str,
        addr: IpAddr,
    ) -> Result<PendingToken, SignInError> {
        let now = self.clock.unix();
        let account = account_key(username);
        let address = address_key(addr);
        let who = Who { account: Some(&account), address: &address, addr };
        self.locked(who.account, who.address, now)?;
        if username.is_empty()
            || username.len() > MAX_USERNAME_BYTES
            || password.is_empty()
            || password.len() > MAX_PASSWORD_BYTES
        {
            return Err(self.fail(Stage::Password, None, who, now, SignInError::Invalid));
        }
        if !self.has_room(now) {
            return Err(self.fail(Stage::Password, None, who, now, SignInError::Busy));
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
                    account_key: account.clone(),
                    material: UnlockMaterial { kek: verified.kek, wiped: self.wiped.clone() },
                };
                let mut map = self.lock();
                if map.len() >= self.max_pending {
                    Self::sweep(&mut map, now, self.pending_secs());
                }
                if map.len() >= self.max_pending {
                    drop(map);
                    return Err(self.fail(Stage::Password, None, who, now, SignInError::Busy));
                }
                map.insert(hash, entry);
                Ok(token)
            }
            Err(PasswordError::InvalidCredentials) => Err(self.fail(Stage::Password, known, who, now, SignInError::Invalid)),
            Err(PasswordError::Busy) => Err(self.fail(Stage::Password, None, who, now, SignInError::Busy)),
            Err(_) => Err(SignInError::Internal),
        }
    }

    // ---- the second step ----

    /// The second step: checks the six-digit code of the account behind `token` and, only if it is right and unused, makes the
    /// session (method password and code), clears the failures of the name and ends the half-signed-in state. The state is
    /// gone after every outcome that counts: success, the last wrong code, a lock of the name and expiry. A wrong code counts
    /// as a failure of the name and of the address. `device` is a short summary of the browser.
    pub fn finish_totp(&self, token: &str, code: &str, addr: IpAddr, device: &str) -> Result<Grant, SignInError> {
        let now = self.clock.unix();
        let address = address_key(addr);
        self.locked(None, &address, now)?;
        let anonymous = Who { account: None, address: &address, addr };
        let Ok(hash) = parse_token(token) else {
            return Err(self.fail(Stage::Totp, None, anonymous, now, SignInError::Invalid));
        };
        let Some((account, key)) = self.reserve(&hash, now) else {
            return Err(self.fail(Stage::Totp, None, anonymous, now, SignInError::Invalid));
        };
        let who = Who { account: Some(&key), address: &address, addr };
        if let Err(locked) = self.locked(who.account, who.address, now) {
            // The name was locked after its first step; the state is not kept for a person who cannot go on.
            self.discard(&hash);
            return Err(locked);
        }
        let checked = self.store.write(|tx| Ok(totp::verify_and_consume(tx, &self.sealer, account, code, now)));
        match checked {
            Ok(Ok(())) => {}
            Ok(Err(TotpError::InvalidCode)) => {
                self.spend_attempt(&hash);
                return Err(self.fail(Stage::Totp, Some(account), who, now, SignInError::Invalid));
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
                // Only a finished sign-in clears the name; the address is never cleared by a success.
                if self.throttle.clear_account(&key).is_err() {
                    log::event(Level::Error, "signin.throttle_failed", &[]);
                }
                self.record(
                    &Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(account), Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(PASSWORD_METHOD.as_str())),
                );
                Ok(Grant { session, unlock: entry.material })
            }
            Err(_) => Err(self.fail(Stage::Totp, Some(account), who, now, SignInError::Invalid)),
        }
    }

    // ---- step-up ----

    /// Proves again, for a sensitive action, that the person at the keyboard knows the password of the session's own account:
    /// the same gate, the same throttle and the same single answer as the first step. A right password moves the session's
    /// `last_auth` to now and writes one `session.step_up` line; a wrong one writes `signin.fail` with step `step_up`, counts
    /// under the name and the address and leaves the session as it was. A session that was not made with the password (a
    /// passkey or single sign-on) is [`SignInError::Invalid`] before anything is derived or counted: it has nothing to prove
    /// with here.
    pub async fn step_up_password(&self, session: &mut Session, password: &str, addr: IpAddr) -> Result<(), SignInError> {
        if session.method != PASSWORD_METHOD {
            return Err(SignInError::Invalid);
        }
        let now = self.clock.unix();
        let account = session.account_id;
        let found = self.store.read(|tx| {
            let Some(found) = tx.account_get(account)?.filter(|a| a.state == AccountState::Active) else {
                return Ok(None);
            };
            Ok(Some((found.username.clone(), lookup_password(tx, &found.username)?)))
        })?;
        let Some((username, stored)) = found else {
            return Err(SignInError::Invalid);
        };
        let key = account_key(&username);
        let address = address_key(addr);
        let who = Who { account: Some(&key), address: &address, addr };
        self.locked(who.account, who.address, now)?;
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return Err(self.fail(Stage::StepUp, Some(account), who, now, SignInError::Invalid));
        }
        match verify_password(&self.gate, stored, password).await {
            Ok(verified) if verified.account == account => {}
            Ok(_) | Err(PasswordError::InvalidCredentials) => {
                return Err(self.fail(Stage::StepUp, Some(account), who, now, SignInError::Invalid));
            }
            Err(PasswordError::Busy) => return Err(self.fail(Stage::StepUp, None, who, now, SignInError::Busy)),
            Err(_) => return Err(SignInError::Internal),
        }
        self.sessions.reauth(session, now).map_err(|_| SignInError::Internal)?;
        self.record(
            &Event::new(Action::SessionStepUp, Outcome::Ok, Actor::Account(account), Some(addr))
                .detail(DetailKey::Method, DetailValue::Text(PASSWORD_METHOD.as_str())),
        );
        Ok(())
    }

    // ---- locks ----

    /// Lifts the lock of `target`'s name: an active administrator `actor` clears the failures counted under the name and the
    /// next attempt is not throttled on its account. The address counters are not touched. One `account.unlock` line is
    /// written for every call, also when the name was not locked. Anyone who is not an active administrator, and a target that
    /// does not exist, gets [`SignInError::Invalid`] (and a line saying so).
    pub fn unlock_signin(&self, actor: AccountId, addr: Option<IpAddr>, target: AccountId) -> Result<(), SignInError> {
        let found = self.store.read(|tx| {
            let admin = tx.account_get(actor)?.is_some_and(|a| a.state == AccountState::Active && a.role == Role::Admin);
            let name = tx.account_get(target)?.filter(|a| a.state != AccountState::Deleted).map(|a| a.username);
            Ok((admin, name))
        })?;
        let (Some(name), true) = (found.1, found.0) else {
            self.record(
                &Event::new(Action::AccountUnlock, Outcome::Fail, Actor::Account(actor), addr)
                    .target(target)
                    .detail(DetailKey::Reason, DetailValue::Text("invalid")),
            );
            return Err(SignInError::Invalid);
        };
        self.throttle.clear_account(&account_key(&name))?;
        self.record(&Event::new(Action::AccountUnlock, Outcome::Ok, Actor::Account(actor), addr).target(target));
        Ok(())
    }

    /// Whether the password path of the name is locked at `now`. A name that does not exist is answered like one that does.
    pub fn is_locked(&self, username: &str, now: i64) -> Result<bool, SignInError> {
        Ok(self.throttle.is_locked(username, now)?)
    }

    /// Unix seconds until which the password path of the name is locked, or `None` when it is not.
    pub fn locked_until(&self, username: &str, now: i64) -> Result<Option<i64>, SignInError> {
        Ok(self.throttle.locked_until(username, now)?)
    }

    /// Removes what has ended: throttle state whose window is over and which is not locked, and pending states past their
    /// time (their material is wiped). At most `max_rows` of each kind per call; a caller that gets `max_rows` back calls
    /// again. A lock is never removed here.
    pub fn purge(&self, now: i64, max_rows: usize) -> Result<Purged, SignInError> {
        let throttle = self.throttle.purge(now, max_rows)?;
        let secs = self.pending_secs();
        let mut map = self.lock();
        let old: Vec<TokenHash> = map
            .iter()
            .filter(|(_, e)| !e.in_flight && now > e.created.saturating_add(secs))
            .map(|(hash, _)| *hash)
            .take(max_rows)
            .collect();
        for hash in &old {
            map.remove(hash);
        }
        Ok(Purged { throttle, pending: old.len() })
    }
}

/// What a purge removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Purged {
    /// Throttle rows.
    pub throttle: usize,
    /// Pending states.
    pub pending: usize,
}

/// A new pending token and the hash it is kept under.
fn new_token() -> Result<(PendingToken, TokenHash), SignInError> {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let bytes = Zeroizing::new(random::<32>().map_err(|_| SignInError::Internal)?);
    let value = PendingToken(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])));
    Ok((value, crate::store::hash_token(&bytes[..])))
}
