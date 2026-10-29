//! Sessions: a random value in a cookie, its hash in the database.
//!
//! A session value is 32 random bytes written as 43 base64url characters. Only the SHA-256 of the 32 bytes is stored, so a
//! copy of the database cannot be used to sign in. Every sign-in makes a new value; no function takes a value from a request
//! and turns it into a session.
//!
//! The lifetimes are exact to the second. A session made at `t` with an idle time of `i` and an absolute time of `a` is valid
//! at `last_seen + i` and refused one second later, and valid at `t + a` and refused one second later, whatever the activity.
//! Lookups only read; the row is written when it ends, when it is touched (at most every 30 seconds) and when the person
//! signs in again for a sensitive action.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use zeroize::Zeroizing;

use crate::clock::Clock;
use crate::config::Sessions;
use crate::store::{AccountId, AccountState, NewSession, Session, Store, StoreError, TokenHash, hash_token};
use crate::unlock::random;

pub use crate::store::SignInMethod as Method;

/// The bytes of a session value before encoding.
const TOKEN_BYTES: usize = 32;

/// The characters of a session value.
pub const TOKEN_CHARS: usize = 43;

/// The most sessions an account has at once. A new one beyond that ends the one seen least recently.
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 20;

/// The bytes of a session handle: the first bytes of the stored hash.
pub const HANDLE_BYTES: usize = 12;

/// The characters of a session handle.
pub const HANDLE_CHARS: usize = 16;

/// The fewest seconds between two writes of `last_seen` for one session.
pub const TOUCH_INTERVAL_SECS: i64 = 30;

/// What the `Debug` text of a session value prints.
const TOKEN_DEBUG: &str = "SessionToken(..)";

/// Why a session was refused or could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The value is not a session: wrong shape, or no such session.
    Unknown,
    /// The session is past its idle or its absolute lifetime. Its row is gone.
    Expired,
    /// The account is disabled or deleted. The session row is gone.
    AccountDisabled,
    /// The operating system gave no random bytes.
    Random,
    /// The store failed.
    Store(StoreError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Unknown => f.write_str("there is no such session"),
            SessionError::Expired => f.write_str("the session has ended"),
            SessionError::AccountDisabled => f.write_str("the account cannot sign in"),
            SessionError::Random => f.write_str("the operating system gave no random bytes"),
            SessionError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<StoreError> for SessionError {
    fn from(e: StoreError) -> SessionError {
        SessionError::Store(e)
    }
}

/// The value of a session cookie. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct SessionToken(Zeroizing<String>);

impl SessionToken {
    /// The 43 characters, for the cookie.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_DEBUG)
    }
}

/// A session that was just made, with the value to put in the cookie.
#[derive(Debug)]
pub struct Created {
    /// The cookie value.
    pub value: SessionToken,
    /// The row.
    pub session: Session,
}

/// The sessions of the server.
pub struct SessionManager {
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    lifetimes: Sessions,
}

impl fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionManager(..)")
    }
}

fn expiry(created: i64, lifetime: std::time::Duration) -> i64 {
    created.saturating_add(i64::try_from(lifetime.as_secs()).unwrap_or(i64::MAX))
}

impl SessionManager {
    /// A manager over `store` with the lifetimes of the configuration.
    pub fn new(store: Arc<Store>, clock: Arc<dyn Clock>, lifetimes: Sessions) -> SessionManager {
        SessionManager { store, clock, lifetimes }
    }

    /// The time of the manager's clock in unix seconds.
    pub fn now(&self) -> i64 {
        self.clock.unix()
    }

    /// Makes a session for `account` after a sign-in by `method` at `now`, with a new random value. `device` is a short
    /// summary of the browser (control characters are dropped, it is cut to 64 characters); `addr` is the client address as the
    /// edge parsed it. An account that already has [`MAX_SESSIONS_PER_ACCOUNT`] sessions loses the one it used least recently.
    /// An account that is not active gets no session.
    ///
    /// This is the only way to make a session, and it takes no cookie value: a value a browser presents is never adopted.
    pub fn create(
        &self,
        account: AccountId,
        method: Method,
        now: i64,
        device: &str,
        addr: Option<IpAddr>,
    ) -> Result<Created, SessionError> {
        let (value, bytes) = generate_token()?;
        let made = self.store.write(|tx| {
            match tx.account_get(account)? {
                Some(found) if found.state == AccountState::Active => {}
                _ => return Ok(Err(SessionError::AccountDisabled)),
            }
            let mut sessions = tx.session_list(account)?;
            while sessions.len() >= MAX_SESSIONS_PER_ACCOUNT {
                // The list is oldest first, so of equal times the older goes.
                let Some(oldest) = (0..sessions.len()).min_by_key(|i| sessions[*i].last_seen) else { break };
                let gone = sessions.remove(oldest);
                tx.session_delete(&gone.id_hash)?;
            }
            let session = tx.session_create(&NewSession {
                id_hash: hash_token(&bytes[..]),
                account_id: account,
                method,
                now,
                absolute_expires: expiry(now, self.lifetimes.absolute),
                device,
                addr: addr.unwrap_or(IpAddr::from([0u8; 4])),
            })?;
            Ok(Ok(session))
        })??;
        Ok(Created { value, session: made })
    }

    /// The session behind a cookie value, if it is alive at `now`. A text that is not exactly 43 characters of the base64url
    /// alphabet is refused without a call to the store. A session past its idle time or its absolute time, or whose account is
    /// not active, is deleted and refused.
    pub fn lookup(&self, value: &str, now: i64) -> Result<Session, SessionError> {
        let hash = parse_token(value)?;
        let (session, account) = self.store.read(|tx| {
            let Some(session) = tx.session_get(&hash)? else { return Ok((None, None)) };
            let account = tx.account_get(session.account_id)?;
            Ok((Some(session), account))
        })?;
        let session = session.ok_or(SessionError::Unknown)?;
        let idle_ends = expiry(session.last_seen, self.lifetimes.idle);
        if now > idle_ends || now > session.absolute_expires {
            self.store.write(|tx| tx.session_delete(&hash))?;
            return Err(SessionError::Expired);
        }
        if !matches!(account, Some(ref found) if found.state == AccountState::Active) {
            self.store.write(|tx| tx.session_delete(&hash))?;
            return Err(SessionError::AccountDisabled);
        }
        Ok(session)
    }

    /// The sessions of an account that are alive at `now`, oldest first. A session past its idle or its absolute time is left
    /// out (it is removed the next time it is looked up, or by a purge).
    pub fn list_live(&self, account: AccountId, now: i64) -> Result<Vec<Session>, SessionError> {
        let rows = self.store.read(|tx| tx.session_list(account))?;
        Ok(rows
            .into_iter()
            .filter(|session| now <= expiry(session.last_seen, self.lifetimes.idle) && now <= session.absolute_expires)
            .collect())
    }

    /// Ends the session of `account` that `handle` names; answers whether there was one. A text that is not a handle, and a
    /// handle of another account's session, are `false` and change nothing. One statement matches the account and the handle.
    pub fn end_by_handle(&self, account: AccountId, handle: &str) -> Result<bool, SessionError> {
        let Some(prefix) = parse_handle(handle) else { return Ok(false) };
        Ok(self.store.write(|tx| tx.session_delete_by_prefix(account, &prefix))?)
    }

    /// Records activity: writes `last_seen = now` when the stored value is at least 30 seconds old, and answers whether it
    /// wrote. The session value is updated to match.
    pub fn touch(&self, session: &mut Session, now: i64) -> Result<bool, SessionError> {
        if now.saturating_sub(session.last_seen) < TOUCH_INTERVAL_SECS {
            return Ok(false);
        }
        if !self.store.write(|tx| tx.session_touch(&session.id_hash, now))? {
            return Err(SessionError::Unknown);
        }
        session.last_seen = session.last_seen.max(now);
        Ok(true)
    }

    /// Whether the person signed in (or signed in again) recently enough for a sensitive action: `now - last_auth` is at most
    /// the step-up time of the configuration.
    pub fn step_up_fresh(&self, session: &Session, now: i64) -> bool {
        let limit = i64::try_from(self.lifetimes.step_up.as_secs()).unwrap_or(i64::MAX);
        now.saturating_sub(session.last_auth) <= limit
    }

    /// Records a fresh sign-in check at `now` (a step-up); `last_auth` only moves forward.
    pub fn reauth(&self, session: &mut Session, now: i64) -> Result<(), SessionError> {
        if !self.store.write(|tx| tx.session_set_last_auth(&session.id_hash, now))? {
            return Err(SessionError::Unknown);
        }
        session.last_auth = session.last_auth.max(now);
        Ok(())
    }

    /// Ends one session; answers whether it was still there.
    pub fn end(&self, session: &Session) -> Result<bool, SessionError> {
        Ok(self.store.write(|tx| tx.session_delete(&session.id_hash))?)
    }

    /// Ends every session of an account; answers how many there were.
    pub fn end_all(&self, account: AccountId) -> Result<usize, SessionError> {
        Ok(self.store.write(|tx| tx.session_delete_all(account))?)
    }

    /// Removes sessions that are past their idle or absolute time at `now`, at most `max_rows` of them; answers how many it
    /// removed. A caller that gets `max_rows` back calls again.
    pub fn purge(&self, now: i64, max_rows: usize) -> Result<usize, SessionError> {
        let idle = i64::try_from(self.lifetimes.idle.as_secs()).unwrap_or(i64::MAX);
        // The store removes a session at `time <= now`; a session here is still valid at its last second, so the store is
        // asked about the second before.
        Ok(self.store.write(|tx| tx.sessions_purge(now.saturating_sub(1), idle, max_rows))?)
    }
}

/// The handle of a session: the first 12 bytes of its stored hash in 16 base64url characters. It names a row of one account
/// and cannot be used as the cookie value (which is 43 characters and the preimage of the hash).
pub fn handle_of(session: &Session) -> String {
    URL_SAFE_NO_PAD.encode(&session.id_hash[..HANDLE_BYTES])
}

/// The 12 bytes a handle text stands for, or `None` for a text that is not exactly 16 characters of the base64url alphabet.
pub fn parse_handle(text: &str) -> Option<[u8; HANDLE_BYTES]> {
    if text.len() != HANDLE_CHARS || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    let mut bytes = [0u8; HANDLE_BYTES];
    let written = URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut bytes).ok()?;
    (written == HANDLE_BYTES).then_some(bytes)
}

/// A new random value and its 32 bytes (wiped when dropped).
pub fn generate_token() -> Result<(SessionToken, Zeroizing<[u8; TOKEN_BYTES]>), SessionError> {
    let bytes = Zeroizing::new(random::<TOKEN_BYTES>().map_err(|_| SessionError::Random)?);
    let value = SessionToken(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])));
    Ok((value, bytes))
}

/// The hash a cookie value is stored under, or [`SessionError::Unknown`] for a text that is not exactly 43 characters of the
/// base64url alphabet that decode to 32 bytes. It looks at nothing but the text.
pub fn parse_token(value: &str) -> Result<TokenHash, SessionError> {
    if value.len() != TOKEN_CHARS || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(SessionError::Unknown);
    }
    let mut bytes = Zeroizing::new([0u8; TOKEN_BYTES]);
    let written = URL_SAFE_NO_PAD.decode_slice(value.as_bytes(), &mut bytes[..]).map_err(|_| SessionError::Unknown)?;
    if written != TOKEN_BYTES {
        return Err(SessionError::Unknown);
    }
    Ok(hash_token(&bytes[..]))
}
