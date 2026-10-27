//! Sessions: a random value in a cookie, its hash in the database.
//!
//! A session value is 32 random bytes written as 43 base64url characters. Only the SHA-256 of the 32 bytes is stored, so a
//! copy of the database cannot be used to sign in. Every sign-in makes a new value; no function takes a value from a request
//! and turns it into a session.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use zeroize::Zeroizing;

use crate::clock::Clock;
use crate::config::Sessions;
use crate::store::{AccountId, NewSession, Session, Store, StoreError, hash_token};
use crate::unlock::random;

pub use crate::store::SignInMethod as Method;

/// The bytes of a session value before encoding.
const TOKEN_BYTES: usize = 32;

/// The characters of a session value.
pub const TOKEN_CHARS: usize = 43;

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

    /// Makes a session for `account` after a sign-in by `method` at `now`.
    pub fn create(
        &self,
        account: AccountId,
        method: Method,
        now: i64,
        device: &str,
        addr: Option<IpAddr>,
    ) -> Result<Created, SessionError> {
        let bytes = Zeroizing::new(random::<TOKEN_BYTES>().map_err(|_| SessionError::Random)?);
        let value = SessionToken(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])));
        let session = self.store.write(|tx| {
            tx.session_create(&NewSession {
                id_hash: hash_token(&bytes[..]),
                account_id: account,
                method,
                now,
                absolute_expires: expiry(now, self.lifetimes.absolute),
                device,
                addr: addr.unwrap_or(IpAddr::from([0u8; 4])),
            })
        })?;
        Ok(Created { value, session })
    }

    /// The session behind a cookie value, if it is still alive at `now`.
    pub fn lookup(&self, value: &str, now: i64) -> Result<Session, SessionError> {
        let hash = parse_token(value)?;
        let session = self.store.read(|tx| tx.session_get(&hash))?.ok_or(SessionError::Unknown)?;
        let idle_ends = expiry(session.last_seen, self.lifetimes.idle);
        if now > idle_ends || now > session.absolute_expires {
            return Err(SessionError::Expired);
        }
        Ok(session)
    }
}

/// The hash of a cookie value, or [`SessionError::Unknown`] for a text that is not exactly 43 characters of the base64url
/// alphabet that decodes to 32 bytes.
fn parse_token(value: &str) -> Result<[u8; 32], SessionError> {
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
